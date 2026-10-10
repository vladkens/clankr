#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effort {
  Low,
  Medium,
  High,
  Xhigh,
  Max,
  Ultra,
}

impl Effort {
  pub const ALL: &[Self] =
    &[Self::Low, Self::Medium, Self::High, Self::Xhigh, Self::Max, Self::Ultra];

  pub fn name(self) -> &'static str {
    match self {
      Self::Low => "low",
      Self::Medium => "medium",
      Self::High => "high",
      Self::Xhigh => "xhigh",
      Self::Max => "max",
      Self::Ultra => "ultra",
    }
  }

  pub fn description(self) -> &'static str {
    match self {
      Self::Low => "Light reasoning",
      Self::Medium => "Balanced reasoning",
      Self::High => "Deeper reasoning",
      Self::Xhigh => "Extra reasoning",
      Self::Max => "Extended reasoning",
      Self::Ultra => "Most reasoning",
    }
  }

  pub fn parse(name: &str) -> Option<Self> {
    Self::ALL.iter().copied().find(|effort| effort.name() == name)
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Model {
  pub name: &'static str,
  pub description: &'static str,
  pub provider: &'static str,
  pub efforts: &'static [Effort],
  pub default_effort: Option<Effort>,
  pub fast: bool,
}

// Codex capabilities match its current model catalog. Echo and Claude remain UI fixtures.
pub const MODELS: &[Model] = &[
  Model {
    name: "echo",
    description: "Local echo",
    provider: "Mock",
    efforts: Effort::ALL,
    default_effort: Some(Effort::Medium),
    fast: true,
  },
  Model {
    name: "gpt-6.1-sol",
    description: "Coding and everyday work",
    provider: "OpenAI",
    efforts: Effort::ALL,
    default_effort: Some(Effort::Low),
    fast: true,
  },
  Model {
    name: "gpt-6-astra",
    description: "Complex tasks",
    provider: "OpenAI",
    efforts: Effort::ALL,
    default_effort: Some(Effort::Low),
    fast: true,
  },
  Model {
    name: "gpt-6-luna",
    description: "Quick everyday tasks",
    provider: "OpenAI",
    efforts: &[Effort::Low, Effort::Medium, Effort::High, Effort::Xhigh, Effort::Max],
    default_effort: Some(Effort::Medium),
    fast: true,
  },
  Model {
    name: "opus",
    description: "Complex and long-running tasks",
    provider: "Anthropic",
    efforts: &[Effort::Low, Effort::Medium, Effort::High, Effort::Xhigh, Effort::Max],
    default_effort: Some(Effort::Xhigh),
    fast: true,
  },
  Model {
    name: "sonnet",
    description: "Everyday coding tasks",
    provider: "Anthropic",
    efforts: &[Effort::Low, Effort::Medium, Effort::High],
    default_effort: Some(Effort::Medium),
    fast: false,
  },
  Model {
    name: "haiku",
    description: "Quick answers",
    provider: "Anthropic",
    efforts: &[],
    default_effort: None,
    fast: false,
  },
];

// https://platform.claude.com/docs/en/models/overview
pub const LIVE_MODELS: &[Model] = &[
  MODELS[1],
  MODELS[2],
  MODELS[3],
  Model {
    name: "claude-opus-5-5",
    description: "Long-running coding and complex tasks",
    provider: "Anthropic",
    efforts: &[Effort::Low, Effort::Medium, Effort::High, Effort::Xhigh, Effort::Max],
    default_effort: Some(Effort::Medium),
    fast: true,
  },
  Model {
    name: "claude-sonnet-5-5",
    description: "Everyday coding tasks",
    provider: "Anthropic",
    efforts: &[Effort::Low, Effort::Medium, Effort::High, Effort::Xhigh, Effort::Max],
    default_effort: Some(Effort::High),
    fast: false,
  },
  Model {
    name: "claude-haiku-5-5",
    description: "Quick answers",
    provider: "Anthropic",
    efforts: &[Effort::Low, Effort::Medium, Effort::High, Effort::Xhigh, Effort::Max],
    default_effort: Some(Effort::Medium),
    fast: false,
  },
];

#[cfg(test)]
impl Model {
  pub fn find(name: &str) -> Option<&'static Self> {
    MODELS.iter().find(|model| model.name == name)
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
  pub model: &'static Model,
  pub effort: Option<Effort>,
  pub fast: bool,
}

impl Default for Settings {
  fn default() -> Self {
    let model = &MODELS[0];
    Self { model, effort: model.default_effort, fast: model.fast }
  }
}

impl Settings {
  pub fn select(&mut self, model: &'static Model) {
    self.model = model;
    self.effort =
      self.effort.filter(|effort| model.efforts.contains(effort)).or(model.default_effort);
    self.fast &= model.fast;
  }

  pub fn step(&mut self, up: bool) {
    let Some(index) = self.model.efforts.iter().position(|&effort| Some(effort) == self.effort)
    else {
      return;
    };

    let index =
      if up { (index + 1).min(self.model.efforts.len() - 1) } else { index.saturating_sub(1) };
    self.effort = Some(self.model.efforts[index]);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn changing_models_preserves_compatible_settings_and_uses_defaults_otherwise() {
    let mut settings = Settings { effort: Some(Effort::Max), ..Settings::default() };
    settings.select(Model::find("gpt-6.1-sol").unwrap());
    assert_eq!(settings.effort, Some(Effort::Max));
    assert!(settings.fast);
    settings.select(Model::find("sonnet").unwrap());
    assert_eq!(settings.effort, Some(Effort::Medium));
    assert!(!settings.fast);
    for _ in 0..10 {
      settings.step(true);
    }
    assert_eq!(settings.effort, Some(Effort::High));
    for _ in 0..10 {
      settings.step(false);
    }
    assert_eq!(settings.effort, Some(Effort::Low));
    settings.select(Model::find("haiku").unwrap());
    settings.step(true);
    settings.step(false);
    assert_eq!(settings.effort, None);
    settings.select(Model::find("opus").unwrap());
    assert_eq!(settings.effort, Some(Effort::Xhigh));
    assert!(!settings.fast);
  }
}
