//! The content layer registry (roadmap Extension 1 §7.1).
//!
//! Four features in the roadmap are "a user-supplied layer the scene
//! draws": the sky (§3.1a), the grass band (§2.1), the custom message
//! (§1.1) and the weather (§3.6). They are defined **once** here, so the
//! scene resolves them in one place and a settings panel renders one list.
//!
//! This is the "content concept" part 2 of the audit predicted, and it is
//! the reason the four features cost less together than apart.

use crate::config::Config;

/// What a content layer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    /// The procedural sky gradient.
    Sky,
    /// The grass band (the built-in one or a drawn tile).
    Grass,
    /// The custom on-screen message.
    Message,
    /// The weather.
    Weather,
}

impl ContentKind {
    /// The layer's own name, for the settings list and the CLI.
    pub fn as_str(&self) -> &'static str {
        match self {
            ContentKind::Sky => "sky",
            ContentKind::Grass => "grass",
            ContentKind::Message => "message",
            ContentKind::Weather => "weather",
        }
    }

    /// Every layer kind, in draw order.
    pub fn all() -> [ContentKind; 4] {
        [
            ContentKind::Sky,
            ContentKind::Grass,
            ContentKind::Message,
            ContentKind::Weather,
        ]
    }
}

/// One layer the scene draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentLayer {
    /// What the layer is.
    pub kind: ContentKind,
    /// Whether the layer draws at all.
    pub visible: bool,
}

/// The layers the scene draws, in draw order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentLayers {
    layers: Vec<ContentLayer>,
}

impl ContentLayers {
    /// The layers a configuration resolves to.
    ///
    /// The grass band is always visible — the parade needs ground — but a
    /// custom tile may replace it. The sky, the message and the weather
    /// are visible only when they carry something.
    pub fn from_config(cfg: &Config) -> Self {
        let layers = ContentKind::all()
            .into_iter()
            .map(|kind| ContentLayer {
                kind,
                visible: layer_visible(cfg, kind),
            })
            .collect();
        Self { layers }
    }

    /// True when the layer of `kind` draws.
    pub fn is_visible(&self, kind: ContentKind) -> bool {
        self.layers
            .iter()
            .find(|layer| layer.kind == kind)
            .map(|layer| layer.visible)
            .unwrap_or(false)
    }

    /// The visible layers, in draw order.
    pub fn visible(&self) -> Vec<ContentKind> {
        self.layers
            .iter()
            .filter(|layer| layer.visible)
            .map(|layer| layer.kind)
            .collect()
    }
}

/// True when the layer of `kind` draws.
fn layer_visible(cfg: &Config, kind: ContentKind) -> bool {
    match kind {
        ContentKind::Sky => cfg.sky.enabled,
        ContentKind::Grass => true,
        ContentKind::Message => !cfg.message.text.trim().is_empty(),
        ContentKind::Weather => cfg.weather.mode != crate::config::WeatherMode::Off,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MessageConfig, SkyConfig, WeatherConfig, WeatherMode};

    #[test]
    fn the_default_config_shows_the_sky_above_the_grass() {
        // The sky is on by default (audit 2026-10-23 §F-049): the ramp is
        // the shipped look, and the grass is always there.
        let layers = ContentLayers::from_config(&Config::default());
        assert_eq!(layers.visible(), vec![ContentKind::Sky, ContentKind::Grass]);
    }

    #[test]
    fn the_layers_follow_the_config() {
        let cfg = Config {
            sky: SkyConfig {
                enabled: true,
                time_of_day: 0.5,
            },
            message: MessageConfig {
                text: "HELLO".to_string(),
                ..MessageConfig::default()
            },
            weather: WeatherConfig {
                mode: WeatherMode::Rain,
                ..WeatherConfig::default()
            },
            ..Config::default()
        };
        let layers = ContentLayers::from_config(&cfg);
        assert_eq!(
            layers.visible(),
            vec![
                ContentKind::Sky,
                ContentKind::Grass,
                ContentKind::Message,
                ContentKind::Weather
            ]
        );
    }

    #[test]
    fn a_blank_message_is_not_a_layer() {
        let cfg = Config {
            message: MessageConfig {
                text: "   ".to_string(),
                ..MessageConfig::default()
            },
            ..Config::default()
        };
        let layers = ContentLayers::from_config(&cfg);
        assert!(!layers.is_visible(ContentKind::Message));
    }

    #[test]
    fn every_kind_has_a_name() {
        for kind in ContentKind::all() {
            assert!(!kind.as_str().is_empty());
        }
    }
}
