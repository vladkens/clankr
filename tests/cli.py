#!/usr/bin/env python3
"""Exercise the ordinary clankr executable in a PTY with pyte==0.8.2.

Checks key bytes, transcript and cleanup. pyte does not prove native Ghostty
reflow, physical keyboard bindings, mouse selection or emoji cell widths.
"""

import argparse
import codecs
import errno
import fcntl
import os
from pathlib import Path
import re
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unicodedata

import pyte

ESC = "\x1b"
UP, DOWN, LEFT, RIGHT = (ESC + "[" + key for key in "ABDC")
HOME, END = ESC + "[H", ESC + "[F"
CSI = re.compile(r"\x1b\[([0-?]*)([ -/]*)([@-~])")
OSC = re.compile(r"\x1b\]([^\x07\x1b]*)(?:\x07|\x1b\\)")
STATUS = ESC + "[0mauto · echo · medium · ⚡︎" + ESC + "[0m"
SUMMARY = re.compile(r"\x1b\[0m  (\d+s \(↑\d+ ↓\d+\))\x1b\[0m")

# Keep the controlling session alive after clankr exits. macOS revokes a PTY
# slave when its session leader exits, preventing a subsequent tcgetattr.
# This supervisor does not alter terminal modes or print into the capture.
SUPERVISOR = """
import os, sys
notify, release = map(int, sys.argv[1:3])
child = os.fork()
if child == 0:
    os.close(notify)
    os.close(release)
    os.execv(sys.argv[3], sys.argv[3:])
_, status = os.waitpid(child, 0)
os.write(notify, str(os.waitstatus_to_exitcode(status)).encode())
os.close(notify)
os.read(release, 1)
os.close(release)
"""


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def tokens(text):
    """Strictly decode our output protocol; never discard ANSI wholesale."""
    pos = 0
    while pos < len(text):
        if text[pos] != ESC:
            end = text.find(ESC, pos)
            end = len(text) if end == -1 else end
            value = text[pos:end]
            require(all(not unicodedata.category(c).startswith("C") or c in "\r\n\t\u200d" for c in value),
                    f"Unexpected terminal control: {value!r}")
            yield "text", value
            pos = end
            continue
        match = CSI.match(text, pos)
        if match:
            params, intermediate, final = match.groups()
            require(not intermediate, f"Unexpected CSI intermediate: {match[0]!r}")
            if final in "hl":
                require(params in ("?2004", "?2026"), f"Forbidden terminal mode: {match[0]!r}")
            elif final == "K":
                require(params in ("", "0"), f"Unexpected line erase: {match[0]!r}")
            elif final == "u":
                require(params in (">1", "<1"), f"Unexpected keyboard protocol: {match[0]!r}")
            elif final == "m":
                require(params in ("0", "1", "3", "4", "9"), f"Use terminal default colours: {match[0]!r}")
            else:
                require(final in "ABGm" and re.fullmatch(r"[0-9;]*", params),
                        f"Forbidden/unknown CSI (including screen clear): {match[0]!r}")
            yield "csi", match[0]
            pos = match.end()
            continue
        match = OSC.match(text, pos)
        if match:
            if match[1].startswith("8;"):
                _, params, uri = match[1].split(";", 2)
                require((not params and not uri)
                        or (re.fullmatch(r"id=clankr-\d+-\d+", params)
                            and re.fullmatch(r"[a-zA-Z][a-zA-Z0-9+.-]*:[\x20-\x7e]+", uri)),
                        f"Invalid hyperlink: {match[0]!r}")
            else:
                require(match[1] in ("133;A;redraw=1", "133;B", "133;C", "133;P;k=s"),
                        f"Unexpected OSC: {match[0]!r}")
            yield "osc", match[0]
            pos = match.end()
            continue
        require(text[pos:pos + 2] in (ESC + "7", ESC + "8"),
                f"Unknown or incomplete escape: {text[pos:pos + 40]!r}")
        yield "escape", text[pos:pos + 2]
        pos += 2


def plain(text):
    return "".join(value for kind, value in tokens(text) if kind == "text")


def audit(text):
    controls = list(tokens(text))
    paste, sync, keyboard, link = False, False, False, False
    for kind, value in controls:
        if kind == "osc" and value.startswith(ESC + "]8;"):
            link = bool(OSC.fullmatch(value)[1].split(";", 2)[2])
        if kind == "escape" or (kind == "csi" and value[-1] in "ABGKhlu"):
            require(not link, "Hyperlink leaked into UI cursor/mode changes")
        if kind != "csi":
            continue
        if value == ESC + "[>1u":
            require(not keyboard, "Keyboard enhancement pushed twice")
            keyboard = True
        elif value == ESC + "[<1u":
            require(keyboard, "Keyboard enhancement popped without a push")
            keyboard = False
        elif value == ESC + "[?2004h":
            require(not paste, "Bracketed paste enabled twice")
            paste = True
        elif value == ESC + "[?2004l":
            paste = False
        elif value == ESC + "[?2026h":
            require(not sync, "Nested synchronized update")
            sync = True
        elif value == ESC + "[?2026l":
            sync = False
    require(not paste and not sync and not keyboard and not link, "Terminal modes left enabled")
    return {"bytes": len(text.encode()), "line_erases": text.count(ESC + "[K"),
            "sync_updates": text.count(ESC + "[?2026h"),
            "keyboard_pushes": text.count(ESC + "[>1u"),
            "keyboard_pops": text.count(ESC + "[<1u"),
            "semantic_marks": sum(kind == "osc" for kind, _ in controls)}



class Terminal:
    def __init__(self, binary):
        self.temp = tempfile.TemporaryDirectory(prefix="clankr-tui-")
        root = Path(self.temp.name)
        home, cwd, state = (root / name for name in ("home", "workspace", "state"))
        for directory in (home, cwd, state):
            directory.mkdir()
        self.root = root
        self.size = 119, 53
        self.screen = pyte.HistoryScreen(*self.size, history=10000)
        self.stream = pyte.Stream(self.screen)
        self.output = ""
        self.decoder = codecs.getincrementaldecoder("utf-8")("strict")
        self.master, self.slave = os.openpty()
        self.before = termios.tcgetattr(self.slave)
        self.set_size(*self.size)
        self.notify, notify = os.pipe()
        release, self.release = os.pipe()
        env = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "HOME": str(home),
               "CLANKR_HOME": str(state), "XDG_CONFIG_HOME": str(home / ".config"),
               "TERM": "xterm-256color", "LC_ALL": "en_US.UTF-8"}

        def controlling_terminal():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        try:
            self.proc = subprocess.Popen(
                [sys.executable, "-c", SUPERVISOR, str(notify), str(release), str(binary)],
                cwd=cwd, env=env, stdin=self.slave, stdout=self.slave, stderr=self.slave,
                preexec_fn=controlling_terminal, pass_fds=(notify, release))
        except BaseException:
            for fd in (self.master, self.slave, self.notify, self.release):
                os.close(fd)
            self.temp.cleanup()
            raise
        finally:
            os.close(notify)
            os.close(release)

    @property
    def display(self):
        return "\n".join(row.rstrip() for row in self.screen.display)

    @property
    def transcript(self):
        lines = ["".join(row[x].data for x in sorted(row)).rstrip()
                 for row in self.screen.history.top]
        return "\n".join(lines + [self.display])

    def set_size(self, cols, rows):
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    def read(self, timeout):
        if not select.select([self.master], [], [], max(0, timeout))[0]:
            return False
        try:
            data = os.read(self.master, 65536)
        except OSError as error:
            if error.errno == errno.EIO:
                return False
            raise
        if not data:
            return False
        text = self.decoder.decode(data)
        self.output += text
        # Feed codepoints so pyte's ZWJ limitation does not swallow the remainder.
        for char in text:
            self.stream.feed(char)
        return True

    def until(self, predicate, label, timeout=8):
        deadline = time.monotonic() + timeout
        while not predicate():
            require(self.proc.poll() is None, f"Exited while waiting for {label}")
            require(time.monotonic() < deadline, f"Timed out: {label}\n{self.display}")
            self.read(min(0.1, deadline - time.monotonic()))

    def ready(self):
        self.until(lambda: STATUS in self.output, "initial editor")
        attrs = termios.tcgetattr(self.slave)
        require(not attrs[3] & (termios.ICANON | termios.ECHO), "Raw mode missing")
        require(ESC + "[>1u" in self.output, "Enhanced keys not enabled")
        self.header = self.screen.display[0].rstrip()
        require(self.header.startswith(":: clankr v"), "Missing compact header")
        require(self.screen.display[2].rstrip() == "┃", "Wrong initial input position")
        require(not self.screen.display[3].strip()
                and self.screen.display[4].startswith("auto · echo · medium ·"),
                "Status must sit one blank row below the editor")
        require(self.screen.buffer[0][3].bold, "Header name must be bold")
        for row, col in ((0, 0), (0, 7), (2, 0), (4, 0)):
            cell = self.screen.buffer[row][col]
            require(cell.fg == "default" and cell.bg == "default", "Use terminal default colours")
        require("Enter send" not in self.output and "Ctrl+C quit" not in self.output,
                "Unrequested keyboard hints in the interface")

    def send(self, value):
        data = value.encode()
        while data:
            data = data[os.write(self.master, data):]

    def paste(self, value):
        self.send(ESC + "[200~" + value + ESC + "[201~")

    def menu(self, title):
        self.until(lambda: title in self.display and "esc close" in self.display
                   and self.screen.display[self.screen.cursor.y].startswith("┃"), title)

    def submit(self, expected, model="echo"):
        start = len(self.output)
        self.send("\r")

        def completed():
            match = SUMMARY.search(self.output[start:])
            return (match and ESC + "[0mauto · " + model in self.output[start + match.end():]
                    and self.screen.display[self.screen.cursor.y].strip() == "┃")

        self.until(completed, f"echo of {expected!r}", timeout=max(8, len(expected) / 160 + 3))
        summary = SUMMARY.search(self.output[start:])[1]
        rows = self.transcript.splitlines()
        end = max(i for i, row in enumerate(rows) if row == "  " + summary) - 1
        lines = expected.split("\n")
        for prefix in ("", "┃ "):
            # pyte normalizes combining marks when it builds terminal cells.
            block = [unicodedata.normalize("NFC", (prefix + line.replace("\t", "    ")).rstrip())
                     for line in lines]
            shown = [unicodedata.normalize("NFC", row) for row in rows[end - len(lines):end]]
            require(shown == block, f"Wrong submitted text or echo: {block!r}\n{shown!r}")
            end -= len(lines) + 1
        return summary

    def resize(self, cols, rows):
        start = len(self.output)
        self.size = cols, rows
        self.screen.resize(lines=rows, columns=cols)
        self.set_size(cols, rows)
        self.until(lambda: ESC + "[65535B" in self.output[start:], f"resize {cols}x{rows}")
        self.until(lambda: 0 <= self.screen.cursor.y < rows and 0 <= self.screen.cursor.x < cols,
                   "cursor inside resized terminal")
        require("HISTORY-SEED" not in self.output[start:], "Resize reprinted a completed turn")

    def exit(self, key="\x03\x03"):
        self.send(key)
        self.until(lambda: select.select([self.notify], [], [], 0)[0], "exit")
        while self.read(0):
            pass
        code = int(os.read(self.notify, 32))
        require(code == 0, f"Exit code {code}: {self.output[-1000:]}")
        require(termios.tcgetattr(self.slave) == self.before, "Terminal modes not restored")
        require(self.output.count(ESC + "[<1u") == 1, "Keyboard cleanup missing or duplicated")
        require(not any(file.is_file() for file in self.root.rglob("*")),
                "Echo TUI unexpectedly wrote session or config files")
        audit(self.output)
        os.write(self.release, b"1")
        require(self.proc.wait(timeout=2) == 0, "PTY supervisor failed")

    def close(self):
        try:
            if self.proc.poll() is None:
                os.killpg(self.proc.pid, signal.SIGKILL)
        finally:
            for fd in (self.master, self.slave, self.notify, self.release):
                os.close(fd)
            self.temp.cleanup()
        self.proc.wait(timeout=5)


def tui(binary):
    mock_lifecycle(binary)
    terminal = Terminal(binary)
    try:
        terminal.ready()
        text = "first line\n\nsecond line\n\nthird line"
        terminal.paste(text)
        terminal.until(lambda: "third line" in terminal.display, "multiline reference draft")
        for row in range(2, 7):
            require(terminal.screen.buffer[row][0].data == "┃"
                    and terminal.screen.buffer[row][0].fg == "default",
                    "Input line must continue through blank rows using the default colour")
        require(not terminal.screen.buffer[2][2].bold, "Draft text must not be bold")
        summary = terminal.submit(text)
        expected = ["┃ first line", "┃", "┃ second line", "┃", "┃ third line", "",
                    "first line", "", "second line", "", "third line", "",
                    "  " + summary, "", "┃", ""]
        require([row.rstrip() for row in terminal.screen.display[2:18]] == expected,
                "Conversation spacing differs from the supplied reference")
        require(terminal.screen.display[18].startswith("auto · echo · medium ·")
                and "/272k " in terminal.screen.display[18], "Missing mock context usage")
        require(terminal.screen.buffer[2][2].bold, "Sent input must be bold")
        require(not terminal.screen.buffer[8][2].bold, "Answer text must remain regular")
        require(terminal.screen.buffer[8][2].fg == "default", "Answer must use terminal text colour")
        terminal.paste("HISTORY-SEED")
        terminal.submit("HISTORY-SEED")
        terminal.send("first" + ESC + "[13;2u" + "second" + ESC + "\r" + "third")
        terminal.submit("first\nsecond\nthird")

        # Clear a complete logical line from its middle, then remove its empty row.
        for erase in ("\x15", ESC + "[127;9u"):
            terminal.paste("keep\nremove\nlast")
            terminal.send(UP + LEFT * 2 + erase + erase)
            terminal.submit("keep\nlast")
            for value, move, expected in (("\nnext", HOME + UP, "next"),
                                          ("keep\n", "", "keep")):
                terminal.paste(value)
                terminal.send(move + erase)
                terminal.submit(expected)

        for erase in (ESC + "\x7f", "\x17"):
            terminal.paste("keep 界e\u0301 👩‍💻")
            terminal.send(erase)
            terminal.submit("keep 界e\u0301 ")
        terminal.paste("a界e\u0301👩‍💻z")
        terminal.send(LEFT + "\x7f" + "\x7f")
        terminal.submit("a界z")
        terminal.paste("abcd")
        terminal.send(HOME + RIGHT + ESC + "[3~" + END + "!")
        terminal.submit("acd!")
        terminal.paste("first\nlast")
        terminal.send("\x01!\x05?")
        terminal.submit("first\n!last?")
        for back, forward in ((ESC + "b", ESC + "f"), (ESC + "[1;3D", ESC + "[1;3C")):
            terminal.paste("one two")
            terminal.send(back + "X" + forward + "!")
            terminal.submit("one Xtwo!")
        terminal.paste("clean\r\nline\t\x1b[2J\u202e")
        terminal.submit("clean\nline\t[2J")
        for index in range(20):
            terminal.paste(f"turn-{index:02}")
            terminal.submit(f"turn-{index:02}")
        require(terminal.transcript.count("HISTORY-SEED") == 2, "Completed history changed")
        for index in range(20):
            require(terminal.transcript.count(f"turn-{index:02}") == 2, "Lost or duplicate turn")
        terminal.paste("resize draft 界\nlast")
        terminal.until(lambda: "resize draft" in terminal.display, "draft before resize")
        for size in ((40, 12), (80, 24), (119, 53), (119, 12), (2, 2), (1, 1), (119, 53)):
            terminal.resize(*size)
        terminal.submit("resize draft 界\nlast")
        terminal.paste("next draft")
        terminal.until(lambda: "next draft" in terminal.display, "next draft before exit")
        terminal.exit()
    finally:
        terminal.close()

    terminal = Terminal(binary)
    try:
        terminal.ready()
        start = len(terminal.output)
        terminal.send("\r")
        terminal.exit("\x04")
        require("\r\n\r\n" not in plain(terminal.output[start:]),
                "Empty Enter sent a message")
    finally:
        terminal.close()
    result = subprocess.run([str(binary)], stdin=subprocess.DEVNULL, capture_output=True, timeout=5)
    require(result.returncode == 1 and b"needs a terminal" in result.stderr,
            "Redirected launch did not return a useful error")
    require(b"\x1b" not in result.stdout + result.stderr, "Redirected launch changed terminal modes")
    print("PASS tui: mock Thinking/Generating, blinking activity, tokens/context, cancellation, exit confirmation, reference layout, default colours, echo, multiline keys, Unicode, history, resize, terminal cleanup, non-TTY error")


def mock_lifecycle(binary):
    terminal = Terminal(binary)
    try:
        terminal.ready()
        terminal.paste("hi!")
        start = len(terminal.output)
        terminal.send("\r")
        terminal.until(lambda: "• Thinking (0s) (↑1 ↓0)" in terminal.display, "mock thinking")
        require(terminal.screen.display[2].rstrip() == "┃ hi!", "Submitted prompt moved")
        require(terminal.screen.display[4].startswith("• Thinking"), "Activity differs from cast")
        require(terminal.screen.display[6].rstrip() == "┃", "Draft must remain below activity")
        terminal.until(lambda: terminal.screen.display[4].startswith("  Thinking"), "blinking dot")
        terminal.paste("next draft")
        terminal.send("\r")
        terminal.until(lambda: "Generating (1s)" in terminal.display, "mock generation")
        terminal.until(lambda: "(↑1 ↓1)" in terminal.display, "live token counter")
        require("┃ next draft" in terminal.display, "Generation swallowed the next draft")
        terminal.until(lambda: SUMMARY.search(terminal.output[start:])
                       and "2/272k 0%" in terminal.display, "final mock statistics and status")
        require("(↑1 ↓1)" in SUMMARY.search(terminal.output[start:])[1], "Wrong final counts")
        require("\nhi!\n" in terminal.display and "┃ next draft" in terminal.display,
                "Final answer or next draft lost")
        require("2/272k 0%" in terminal.display, "Context count missing after completion")
        require("Thinking" not in terminal.display and "Generating" not in terminal.display,
                "Activity should disappear after completion")
        require(len(SUMMARY.findall(terminal.output[start:])) == 1, "Busy Enter submitted twice")

        terminal.send("\x15")
        for phase in ("Thinking", "Generating"):
            terminal.paste("cancel this")
            terminal.send("\r")
            terminal.until(lambda: phase in terminal.display, f"cancel during {phase}")
            terminal.paste("preserved")
            terminal.send(ESC)
            terminal.until(lambda: "Cancelled ·" in terminal.display
                           and "Thinking" not in terminal.display
                           and "Generating" not in terminal.display, "cancelled mock request")
            require("┃ preserved" in terminal.display, "Cancellation lost the next draft")
            require("2/272k 0%" in terminal.display, "Cancelled turn changed completed context")
            terminal.send("\x15")

        terminal.send("\x03")
        terminal.until(lambda: "press ctrl+c again to exit" in terminal.display, "first ctrl+c")
        terminal.until(lambda: "press ctrl+c again to exit" not in terminal.display,
                       "exit confirmation timeout")
        terminal.paste("still here")
        terminal.until(lambda: "┃ still here" in terminal.display, "draft after exit timeout")
        terminal.send("\x03")
        terminal.until(lambda: "still here" not in terminal.display
                       and terminal.screen.display[terminal.screen.cursor.y].rstrip() == "┃"
                       and "press ctrl+c again to exit" in terminal.display,
                       "ctrl+c clears the draft and shows the exit hint in an existing session")
        terminal.exit("\x03")
    finally:
        terminal.close()


def menus(binary):
    terminal = Terminal(binary)
    try:
        terminal.ready()
        terminal.send("/copy\r")
        terminal.until(lambda: "* No assistant reply to copy." in terminal.display
                       and terminal.screen.display[terminal.screen.cursor.y].rstrip() == "┃"
                       and "auto · echo · medium" in terminal.display, "copy message above the editor")
        message_row = next(i for i, row in enumerate(terminal.screen.display)
                           if row.startswith("* No assistant reply to copy."))
        require(message_row < terminal.screen.cursor.y, "Clipboard feedback replaced the status")
        terminal.send("draft")
        terminal.until(lambda: "┃ draft" in terminal.display, "typing after copy feedback")
        require("* No assistant reply to copy." in terminal.display,
                "Clipboard feedback disappeared from history")
        terminal.send("\x03")
        terminal.until(lambda: "┃ draft" not in terminal.display
                       and terminal.screen.display[terminal.screen.cursor.y].rstrip() == "┃"
                       and "press ctrl+c again to exit" in terminal.display,
                       "ctrl+c clears the fresh draft and shows the exit hint")
        terminal.exit("\x03")
    finally:
        terminal.close()

    terminal = Terminal(binary)
    try:
        terminal.ready()
        terminal.send("/")
        terminal.menu("Commands 6")
        require("/clear" in terminal.display and "/quit" in terminal.display, "Missing commands")
        require("/fast" in terminal.display and "/copy" in terminal.display, "Missing fast/copy commands")
        require("/help" not in terminal.display and "agent-rules" not in terminal.display,
                "Slash menu must contain commands only, without help")
        require(terminal.screen.display[2].rstrip() == "┃ /", "Menu moved above the editor")
        require(terminal.screen.display[3].startswith("─"), "Missing menu divider")
        terminal.send("cl" + RIGHT)
        terminal.until(lambda: "Commands" not in terminal.display and "┃ /clear" in terminal.display,
                       "Right completes the command without running it")
        terminal.send("\r")
        terminal.until(lambda: terminal.screen.display[0].rstrip() == terminal.header
                       and terminal.screen.display[2].rstrip() == "┃", "clear command")

        terminal.send("huj! $age")
        terminal.menu("Skills 3")
        require("agent-rules" in terminal.display and "/clear" not in terminal.display,
                "Dollar picker must contain skills only")
        terminal.send(DOWN + RIGHT)
        terminal.until(lambda: "Skills" not in terminal.display
                       and "┃ huj! $agent-rules" in terminal.display, "Right completes the picked skill")
        row = terminal.screen.cursor.y
        require(terminal.screen.buffer[row][7].bold, "Picked skill must be visually distinct")
        terminal.send("yes")
        terminal.until(lambda: "┃ huj! $agent-rules yes" in terminal.display, "typing after a skill")
        terminal.send("\x7f" * 5)
        terminal.until(lambda: terminal.screen.display[terminal.screen.cursor.y].rstrip() == "┃ huj!",
                       "whole skill deletion")
        terminal.send("\x15")
        terminal.paste("copy paste $agent-rules? 1221e")
        terminal.until(lambda: "copy paste $agent-rules? 1221e" in terminal.display, "literal paste")
        require("Skills" not in terminal.display, "Paste unexpectedly opened the skill picker")
        row = terminal.screen.cursor.y
        require(not any(cell.bold for cell in terminal.screen.buffer[row].values()),
                "Pasted skill name was activated")
        terminal.send("\x15")
        terminal.paste("$agent-rules")
        terminal.send("\x7f")
        terminal.until(lambda: terminal.screen.display[terminal.screen.cursor.y].rstrip() == "┃ $agent-rule",
                       "pasted names delete as ordinary text")
        terminal.send("\x15$")
        terminal.menu("Skills 8")
        terminal.send(DOWN * 7)
        terminal.until(lambda: "3–8" in terminal.display, "skill list scrolling")
        terminal.send(DOWN)
        terminal.until(lambda: "1–6" in terminal.display, "Down wraps to the first skill")
        terminal.send(UP)
        terminal.until(lambda: "3–8" in terminal.display, "Up wraps to the last skill")
        for cols, rows in ((40, 12), (2, 2), (1, 1), (119, 53)):
            terminal.resize(cols, rows)
        terminal.send("\t")
        terminal.until(lambda: terminal.screen.display[terminal.screen.cursor.y].rstrip() == "┃ $planning",
                       "Tab chooses the skill after resize")
        terminal.send("\x15$zzzz")
        terminal.until(lambda: "No skills found." in terminal.display, "empty result")
        terminal.send("\r")
        terminal.send(ESC)
        terminal.until(lambda: terminal.screen.display[terminal.screen.cursor.y].rstrip() == "┃ $zzzz"
                       and "No skills found." not in terminal.display, "Esc preserves the draft")
        terminal.send("\x15")
        terminal.paste("HISTORY-SEED")
        terminal.submit("HISTORY-SEED")
        terminal.send("/cl\r")
        terminal.until(lambda: terminal.screen.display[0].rstrip() == terminal.header
                       and terminal.screen.display[2].rstrip() == "┃"
                       and "HISTORY-SEED" not in terminal.display, "fresh screen")
        require(terminal.transcript.count("HISTORY-SEED") == 2, "Clear destroyed native scrollback")
        require("/272k" not in terminal.display, "Clear did not reset mock context")

        for phase in ("Thinking", "Generating"):
            terminal.paste("cancel current")
            terminal.send("\r")
            terminal.until(lambda: phase in terminal.display, f"Ctrl+C during {phase}")
            terminal.paste("next draft")
            start = len(terminal.output)
            terminal.send("\x03")
            terminal.until(lambda: "Cancelled ·" in terminal.output[start:], "Ctrl+C cancellation")
            require("again to exit" not in terminal.output[start:], "Active Ctrl+C offered to close the session")
            require("┃ next draft" in terminal.display, "Ctrl+C lost the next draft")
            terminal.send("\x15")

        terminal.paste("old pending")
        terminal.send("\r")
        terminal.until(lambda: "Thinking" in terminal.display, "active clear request")
        terminal.send("$")
        terminal.menu("Skills 8")
        terminal.send(ESC)
        terminal.until(lambda: "Skills 8" not in terminal.display, "close picker while active")
        require("Thinking" in terminal.display, "Esc closed the request instead of the picker")
        terminal.send("\x15/cl\r")
        terminal.until(lambda: terminal.screen.display[0].rstrip() == terminal.header
                       and terminal.screen.display[2].rstrip() == "┃"
                       and "Thinking" not in terminal.display, "clear while active")
        terminal.paste("new conversation")
        terminal.submit("new conversation")
        require("old pending" not in terminal.display, "Old request reappeared after clear")
        terminal.send("\x03")
        terminal.until(lambda: "press ctrl+c again to exit" in terminal.display,
                       "Existing session asks for exit confirmation")
        terminal.send("/clear\r")
        terminal.until(lambda: terminal.screen.display[0].rstrip() == terminal.header
                       and terminal.screen.display[2].rstrip() == "┃"
                       and "/272k" not in terminal.display, "clear before exit")
        start = len(terminal.output)
        terminal.exit("\x03")
        require("again to exit" not in terminal.output[start:],
                "Cleared session asked for exit confirmation")
    finally:
        terminal.close()
    print("PASS menus: separate slash/dollar pickers, filtering, scroll, completion, atomic skill deletion, literal paste, resize, clear, active Ctrl+C, session exit")


def markdown(binary):
    terminal = Terminal(binary)
    source = ("Streaming preview " + "draft remains editable. " * 20 + "PREVIEW-END\n\n"
              "# Heading\n\n**bold** and *italic*, ~~old~~ and `code`.\n\n"
              "- first\n  - nested\n- [x] done\n\n"
              "[site](https://example.org) [src/main.rs](/Users/user/Code/clankr/src/main.rs)\n\n"
              "```rust\nfn main() {\n    println!(\"hello\");\n}\n```\n\n"
              "| name | value |\n| --- | ---: |\n| Makefile | 42 |\n| 界 | 1234 |\n\n"
              "Final paragraph.")
    try:
        terminal.ready()
        terminal.paste(source)
        start = len(terminal.output)
        terminal.send("\r")
        terminal.until(lambda: "Thinking" in terminal.display, "markdown request")
        terminal.paste("next draft")
        terminal.until(lambda: any(row.startswith("Streaming preview")
                                  for row in terminal.screen.display), "unfinished paragraph preview")
        require(not any("PREVIEW-END" in row for row in terminal.screen.display
                        if not row.startswith("┃")), "Paragraph appeared only after its end arrived")
        require("┃ next draft" in terminal.display, "Preview hid the next draft")
        terminal.until(lambda: "Generating" in terminal.display
                       and any(row.rstrip() == "Heading" for row in terminal.screen.display),
                       "heading during streaming")
        require(not SUMMARY.search(terminal.output[start:]), "Markdown appeared only after completion")
        terminal.until(lambda: SUMMARY.search(terminal.output[start:])
                       and "┃ next draft" in terminal.display, "formatted response and next draft",
                       timeout=15)
        rows = terminal.screen.display
        heading = next(i for i, row in enumerate(rows) if row.rstrip() == "Heading")
        require(terminal.screen.buffer[heading][0].bold, "Heading is not bold")
        inline = next(i for i, row in enumerate(rows)
                      if row.rstrip() == "bold and italic, old and code.")
        for word, attr in (("bold", "bold"), ("italic", "italics"), ("old", "strikethrough")):
            col = re.search(rf"\b{word}\b", rows[inline]).start()
            require(getattr(terminal.screen.buffer[inline][col], attr), f"Missing {word} style")
        for text in ("- first", "  - nested", "- [x] done", "site src/main.rs",
                     "─ rust ─────", '    println!("hello");', "name     │ value",
                     "─────────┼──────", "Makefile │    42", "界       │  1234"):
            require(text in terminal.transcript, f"Missing Markdown output: {text}")
        require(not terminal.screen.buffer[inline][rows[inline].index("code")].underscore,
                "Inline code is underlined")
        linked = next(i for i, row in enumerate(rows) if row.rstrip() == "site src/main.rs")
        for label in ("site", "src/main.rs"):
            col = rows[linked].index(label)
            require(all(terminal.screen.buffer[linked][i].underscore
                        for i in range(col, col + len(label))), f"Link is not underlined: {label}")
        require(not terminal.screen.buffer[linked][4].underscore,
                "Link underline leaked into surrounding text")
        for uri in ("https://example.org/", "file:///Users/user/Code/clankr/src/main.rs"):
            require(any(match[1].startswith("8;id=clankr-") and match[1].endswith(";" + uri)
                        for match in OSC.finditer(terminal.output[start:])), f"Missing hyperlink: {uri}")
        require("(https://example.org)" not in terminal.display.split("site src/main.rs", 1)[1],
                "Hyperlink destination duplicated after its label")
        draft = terminal.screen.cursor.y
        require(not terminal.screen.buffer[draft][2].bold
                and not terminal.screen.buffer[draft][2].underscore, "Markdown style leaked into input")
        terminal.send("\x03")
        terminal.until(lambda: "press ctrl+c again to exit" in terminal.display, "exit hint")
        terminal.exit("\x03")
    finally:
        terminal.close()
    print("PASS markdown: streamed heading, nested styles/lists, links, code indentation, table, draft, default colours and terminal cleanup")


def models(binary):
    terminal = Terminal(binary)
    lower, higher = ESC + "[44;5u", ESC + "[46;5u"

    def selected(value):
        terminal.until(lambda: "auto · " + value in terminal.display
                       and "esc close" not in terminal.display
                       and terminal.screen.display[terminal.screen.cursor.y].rstrip() == "┃",
                       "selected " + value)

    try:
        terminal.ready()
        terminal.send("/fast" + RIGHT)
        terminal.until(lambda: "┃ /fast" in terminal.display and "⚡" in terminal.display
                       and "esc close" not in terminal.display, "Fast completion does not toggle")
        terminal.send("\r")
        selected("echo · medium")
        terminal.until(lambda: "⚡" not in terminal.display, "Fast off")
        terminal.send("/fast\r")
        selected("echo · medium · ⚡")
        terminal.send("/model\r")
        terminal.menu("Models 7")
        require("echo ✓" in terminal.display, "Missing current model marker")
        terminal.send(UP)
        terminal.until(lambda: "2–7" in terminal.display, "Up wraps to the last model")
        terminal.send(DOWN)
        terminal.until(lambda: "1–6" in terminal.display, "Down wraps to the first model")
        for cols, rows in ((40, 12), (1, 1), (119, 53)):
            terminal.resize(cols, rows)
        terminal.send("gpt-6.1" + RIGHT)
        terminal.until(lambda: "┃ /model gpt-6.1-sol" in terminal.display
                       and "auto · echo · medium" in terminal.display, "model completion without applying")
        terminal.send("\r")
        selected("gpt-6.1-sol · medium · ⚡")
        terminal.send(higher)
        selected("gpt-6.1-sol · high")
        terminal.send(lower)
        selected("gpt-6.1-sol · medium")
        terminal.send(higher * 10)
        selected("gpt-6.1-sol · ultra")
        terminal.send("/effort\r")
        terminal.menu("Effort 6 · gpt-6.1-sol")
        require("ultra ✓" in terminal.display, "Missing current effort marker")
        terminal.send(DOWN + "\r")
        selected("gpt-6.1-sol · low")

        terminal.send("/model opus")
        terminal.menu("Models 1")
        terminal.send(ESC)
        terminal.until(lambda: "auto · gpt-6.1-sol · low" in terminal.display
                       and "┃ /model opus" in terminal.display, "Esc discards model selection")
        terminal.send("\x15/effort high\r")
        selected("gpt-6.1-sol · high")
        terminal.send("/model sonnet\r")
        selected("sonnet · high")
        require("⚡" not in terminal.display, "Unsupported speed was kept")
        terminal.send("/model opus\r")
        selected("opus · high")
        terminal.send(higher)
        selected("opus · xhigh")
        terminal.send("/model sonnet\r")
        selected("sonnet · medium")

        terminal.send("/model haiku\r")
        selected("haiku")
        terminal.send("/effort\r")
        terminal.menu("Effort 0 · haiku")
        require("haiku has no reasoning effort." in terminal.display,
                "Missing explanation for a model without effort")
        terminal.send(lower + higher + "\r" + ESC)
        terminal.until(lambda: "auto · haiku" in terminal.display
                       and "┃ /effort" in terminal.display, "Unsupported effort stays unset")
        terminal.send("\x15/model opus\r")
        selected("opus · xhigh")
        terminal.send(",.")
        terminal.until(lambda: "┃ ,." in terminal.display, "Plain punctuation still edits the draft")
        terminal.send("\x15")
        terminal.paste("selected model echo")
        terminal.submit("selected model echo", model="opus")
        terminal.send("/clear\r")
        selected("opus · xhigh")
        terminal.until(lambda: "/272k" not in terminal.display, "Clear preserves settings and resets context")
        terminal.exit("\x03")
    finally:
        terminal.close()
    print("PASS models: separate model/effort lists, completion, cancel, cyclic navigation, resize, current markers, compatible settings, default fallback, Ctrl+,/Ctrl+., echo, clear")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, required=True)
    parser.add_argument("--case", choices=["tui", "menus", "models", "markdown"], required=True)
    args = parser.parse_args()
    {"tui": tui, "menus": menus, "models": models, "markdown": markdown}[args.case](args.bin.resolve(strict=True))


if __name__ == "__main__":
    main()
