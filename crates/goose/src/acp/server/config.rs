use super::*;

impl GooseAcpAgent {
    pub(super) async fn on_preferences_read(
        &self,
        req: PreferencesReadRequest,
    ) -> Result<PreferencesReadResponse, agent_client_protocol::Error> {
        let config = self.config()?;
        let keys = if req.keys.is_empty() {
            PREFERENCE_DEFS.iter().map(|def| def.key).collect()
        } else {
            req.keys
        };
        let mut values = Vec::with_capacity(keys.len());

        for key in keys {
            let def = preference_def(key)?;
            let value = match config.get_param::<serde_json::Value>(def.config_key) {
                Ok(value) => value,
                Err(crate::config::ConfigError::NotFound(_)) => serde_json::Value::Null,
                Err(e) => {
                    return Err(agent_client_protocol::Error::internal_error().data(e.to_string()))
                }
            };
            values.push(PreferenceValue { key, value });
        }

        Ok(PreferencesReadResponse { values })
    }

    pub(super) async fn on_preferences_save(
        &self,
        req: PreferencesSaveRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        let config = self.config()?;
        let mut updates = Vec::with_capacity(req.values.len());

        for preference in &req.values {
            let def = preference_def(preference.key)?;
            (def.validate)(&preference.value)?;
            updates.push((def.config_key.to_string(), preference.value.clone()));
        }

        config.set_param_values(&updates).internal_err()?;
        Ok(EmptyResponse {})
    }

    pub(super) async fn on_preferences_remove(
        &self,
        req: PreferencesRemoveRequest,
    ) -> Result<EmptyResponse, agent_client_protocol::Error> {
        let config = self.config()?;
        for key in req.keys {
            let def = preference_def(key)?;
            config.delete(def.config_key).internal_err()?;
        }
        Ok(EmptyResponse {})
    }

    pub(super) async fn on_defaults_read(
        &self,
        _req: DefaultsReadRequest,
    ) -> Result<DefaultsReadResponse, agent_client_protocol::Error> {
        let config = self.config()?;
        Ok(DefaultsReadResponse {
            provider_id: optional_config_string(&config, "GOOSE_PROVIDER")?,
            model_id: optional_config_string(&config, "GOOSE_MODEL")?,
        })
    }

    pub(super) async fn on_defaults_save(
        &self,
        req: DefaultsSaveRequest,
    ) -> Result<DefaultsReadResponse, agent_client_protocol::Error> {
        let provider_id = req.provider_id.trim().to_string();
        if provider_id.is_empty() {
            return Err(
                agent_client_protocol::Error::invalid_params().data("providerId cannot be empty")
            );
        }

        let model_id = req.model_id.and_then(|model| {
            let model = model.trim().to_string();
            (!model.is_empty()).then_some(model)
        });

        let entries = self
            .provider_inventory
            .entries(std::slice::from_ref(&provider_id))
            .await
            .internal_err_ctx("Failed to read provider inventory")?;
        let Some(entry) = entries
            .into_iter()
            .find(|entry| entry.provider_id == provider_id)
        else {
            return Err(agent_client_protocol::Error::invalid_params()
                .data(format!("Unknown provider: {provider_id}")));
        };

        if !entry.configured {
            return Err(agent_client_protocol::Error::invalid_params()
                .data(format!("Provider is not configured: {provider_id}")));
        }

        if let Some(model_id) = model_id.as_deref() {
            let model_exists = entry.default_model == model_id
                || entry.models.iter().any(|model| model.id == model_id);
            if !model_exists {
                return Err(agent_client_protocol::Error::invalid_params().data(format!(
                    "Model '{model_id}' is not available for provider '{provider_id}'"
                )));
            }
        }

        let config = self.config()?;
        config
            .set_param_values(&[(
                "GOOSE_PROVIDER".to_string(),
                serde_json::Value::String(provider_id.clone()),
            )])
            .internal_err_ctx("Failed to save default provider")?;
        if let Some(model_id) = model_id.as_deref() {
            config
                .set_param("GOOSE_MODEL", model_id)
                .internal_err_ctx("Failed to save default model")?;
        } else {
            config
                .delete("GOOSE_MODEL")
                .internal_err_ctx("Failed to clear default model")?;
        }

        Ok(DefaultsReadResponse {
            provider_id: Some(provider_id),
            model_id,
        })
    }
}

struct PreferenceDef {
    key: PreferenceKey,
    config_key: &'static str,
    validate: fn(&serde_json::Value) -> Result<(), agent_client_protocol::Error>,
}

const PREFERENCE_DEFS: &[PreferenceDef] = &[
    PreferenceDef {
        key: PreferenceKey::AutoCompactThreshold,
        config_key: "GOOSE_AUTO_COMPACT_THRESHOLD",
        validate: validate_auto_compact_threshold,
    },
    PreferenceDef {
        key: PreferenceKey::VoiceAutoSubmitPhrases,
        config_key: "VOICE_AUTO_SUBMIT_PHRASES",
        validate: validate_voice_auto_submit_phrases,
    },
    PreferenceDef {
        key: PreferenceKey::VoiceDictationProvider,
        config_key: "VOICE_DICTATION_PROVIDER",
        validate: validate_voice_dictation_provider,
    },
    PreferenceDef {
        key: PreferenceKey::VoiceDictationPreferredMic,
        config_key: "VOICE_DICTATION_PREFERRED_MIC",
        validate: validate_voice_dictation_preferred_mic,
    },
    // ── RUSKY FORK PATCH: personality state (SPEC-156) ──────────────────────
    PreferenceDef {
        key: PreferenceKey::PersonalityState,
        config_key: "RUSKY_PERSONALITY_STATE",
        validate: validate_personality_state,
    },
    // ── /RUSKY FORK PATCH: personality state ────────────────────────────────
    // ── RUSKY FORK PATCH: privacy preferences (SPEC-100) ────────────────────
    PreferenceDef {
        key: PreferenceKey::PrivacyTelemetryCrashReports,
        config_key: "RUSKY_PRIVACY_TELEMETRY_CRASH_REPORTS",
        validate: validate_privacy_bool,
    },
    PreferenceDef {
        key: PreferenceKey::PrivacyDeleteAllLocalData,
        config_key: "RUSKY_PRIVACY_DELETE_ALL_LOCAL_DATA",
        validate: validate_privacy_bool,
    },
    PreferenceDef {
        key: PreferenceKey::PrivacyOutboundDataInventory,
        config_key: "RUSKY_PRIVACY_OUTBOUND_DATA_INVENTORY",
        validate: validate_privacy_bool,
    },
    // ── /RUSKY FORK PATCH: privacy preferences ──────────────────────────────
    // ── RUSKY FORK PATCH: network preferences ────────────────────────────────
    PreferenceDef {
        key: PreferenceKey::NetworkProxyMode,
        config_key: "RUSKY_NETWORK_PROXY_MODE",
        validate: validate_network_proxy_mode,
    },
    PreferenceDef {
        key: PreferenceKey::NetworkProxyUrl,
        config_key: "RUSKY_NETWORK_PROXY_URL",
        validate: validate_network_proxy_url,
    },
    PreferenceDef {
        key: PreferenceKey::NetworkCustomCaBundlePath,
        config_key: "RUSKY_NETWORK_CUSTOM_CA_BUNDLE_PATH",
        validate: validate_network_optional_string,
    },
    PreferenceDef {
        key: PreferenceKey::NetworkAllowSelfSignedCerts,
        config_key: "RUSKY_NETWORK_ALLOW_SELF_SIGNED_CERTS",
        validate: validate_network_bool,
    },
    PreferenceDef {
        key: PreferenceKey::NetworkAllowedDomains,
        config_key: "RUSKY_NETWORK_ALLOWED_DOMAINS",
        validate: validate_network_allowed_domains,
    },
    // ── /RUSKY FORK PATCH: network preferences ───────────────────────────────
    // ── RUSKY FORK PATCH: sound + keyboard preferences (G8) ──────────────────
    PreferenceDef {
        key: PreferenceKey::SoundEnabled,
        config_key: "RUSKY_SOUND_ENABLED",
        validate: validate_g8_bool,
    },
    PreferenceDef {
        key: PreferenceKey::SoundNotificationVolume,
        config_key: "RUSKY_SOUND_NOTIFICATION_VOLUME",
        validate: validate_unit_volume,
    },
    PreferenceDef {
        key: PreferenceKey::SoundHeartbeatChime,
        config_key: "RUSKY_SOUND_HEARTBEAT_CHIME",
        validate: validate_g8_bool,
    },
    PreferenceDef {
        key: PreferenceKey::SoundAutomationCompleteChime,
        config_key: "RUSKY_SOUND_AUTOMATION_COMPLETE_CHIME",
        validate: validate_g8_bool,
    },
    PreferenceDef {
        key: PreferenceKey::SoundErrorChime,
        config_key: "RUSKY_SOUND_ERROR_CHIME",
        validate: validate_g8_bool,
    },
    PreferenceDef {
        key: PreferenceKey::KeyboardGlobalShortcuts,
        config_key: "RUSKY_KEYBOARD_GLOBAL_SHORTCUTS",
        validate: validate_keyboard_global_shortcuts,
    },
    // ── /RUSKY FORK PATCH: sound + keyboard preferences (G8) ─────────────────
];

fn preference_def(
    key: PreferenceKey,
) -> Result<&'static PreferenceDef, agent_client_protocol::Error> {
    PREFERENCE_DEFS
        .iter()
        .find(|def| def.key == key)
        .ok_or_else(|| {
            agent_client_protocol::Error::internal_error()
                .data(format!("Missing preference definition for {key:?}"))
        })
}

fn validate_auto_compact_threshold(
    value: &serde_json::Value,
) -> Result<(), agent_client_protocol::Error> {
    let Some(value) = value.as_f64() else {
        return Err(agent_client_protocol::Error::invalid_params()
            .data("autoCompactThreshold must be a number"));
    };
    if !value.is_finite() || value <= 0.0 || value > 1.0 {
        return Err(agent_client_protocol::Error::invalid_params()
            .data("autoCompactThreshold must be greater than 0 and at most 1"));
    }

    Ok(())
}

fn validate_voice_auto_submit_phrases(
    value: &serde_json::Value,
) -> Result<(), agent_client_protocol::Error> {
    if !value.is_string() {
        return Err(agent_client_protocol::Error::invalid_params()
            .data("voiceAutoSubmitPhrases must be a string"));
    }

    Ok(())
}

fn validate_voice_dictation_provider(
    value: &serde_json::Value,
) -> Result<(), agent_client_protocol::Error> {
    let Some(value) = value.as_str() else {
        return Err(agent_client_protocol::Error::invalid_params()
            .data("voiceDictationProvider must be a string"));
    };
    if !is_supported_voice_dictation_provider(value) {
        return Err(agent_client_protocol::Error::invalid_params()
            .data("voiceDictationProvider is not supported"));
    }

    Ok(())
}

fn validate_voice_dictation_preferred_mic(
    value: &serde_json::Value,
) -> Result<(), agent_client_protocol::Error> {
    let Some(value) = value.as_str() else {
        return Err(agent_client_protocol::Error::invalid_params()
            .data("voiceDictationPreferredMic must be a string"));
    };
    if value.is_empty() {
        return Err(agent_client_protocol::Error::invalid_params()
            .data("voiceDictationPreferredMic must be non-empty"));
    }

    Ok(())
}

// ── RUSKY FORK PATCH: personality + privacy validators ─────────────────────

fn validate_personality_state(
    value: &serde_json::Value,
) -> Result<(), agent_client_protocol::Error> {
    if !value.is_object() {
        return Err(agent_client_protocol::Error::invalid_params()
            .data("personalityState must be a JSON object"));
    }
    Ok(())
}

fn validate_privacy_bool(value: &serde_json::Value) -> Result<(), agent_client_protocol::Error> {
    if !value.is_boolean() {
        return Err(agent_client_protocol::Error::invalid_params().data("expected a boolean value"));
    }
    Ok(())
}

// ── /RUSKY FORK PATCH: personality + privacy validators ────────────────────
// ── RUSKY FORK PATCH: network preferences validators ───────────────────────

fn validate_network_proxy_mode(
    value: &serde_json::Value,
) -> Result<(), agent_client_protocol::Error> {
    let Some(value) = value.as_str() else {
        return Err(agent_client_protocol::Error::invalid_params()
            .data("networkProxyMode must be a string"));
    };
    if !matches!(value, "none" | "auto" | "manual") {
        return Err(agent_client_protocol::Error::invalid_params()
            .data("networkProxyMode must be one of: none, auto, manual"));
    }
    Ok(())
}

fn validate_network_proxy_url(
    value: &serde_json::Value,
) -> Result<(), agent_client_protocol::Error> {
    let Some(value) = value.as_str() else {
        return Err(
            agent_client_protocol::Error::invalid_params().data("networkProxyUrl must be a string")
        );
    };
    if value.is_empty() {
        // Empty clears the override — explicitly allowed.
        return Ok(());
    }
    // Loose URL validation: must parse with a scheme. We don't pin to
    // http/https so on-prem distros can use custom schemes; the proxy
    // client enforces the final shape.
    if url::Url::parse(value).is_err() {
        return Err(
            agent_client_protocol::Error::invalid_params().data("networkProxyUrl must be a URL")
        );
    }
    Ok(())
}

fn validate_network_optional_string(
    value: &serde_json::Value,
) -> Result<(), agent_client_protocol::Error> {
    if !value.is_string() {
        return Err(agent_client_protocol::Error::invalid_params().data("expected a string value"));
    }
    Ok(())
}

fn validate_network_bool(value: &serde_json::Value) -> Result<(), agent_client_protocol::Error> {
    if !value.is_boolean() {
        return Err(agent_client_protocol::Error::invalid_params().data("expected a boolean value"));
    }
    Ok(())
}

fn validate_network_allowed_domains(
    value: &serde_json::Value,
) -> Result<(), agent_client_protocol::Error> {
    let Some(arr) = value.as_array() else {
        return Err(agent_client_protocol::Error::invalid_params()
            .data("networkAllowedDomains must be a JSON array"));
    };
    for entry in arr {
        if !entry.is_string() {
            return Err(agent_client_protocol::Error::invalid_params()
                .data("networkAllowedDomains entries must be strings"));
        }
    }
    Ok(())
}

// ── /RUSKY FORK PATCH: network preferences validators ──────────────────────

// ── RUSKY FORK PATCH: sound + keyboard preferences validators (G8) ────────

fn validate_g8_bool(value: &serde_json::Value) -> Result<(), agent_client_protocol::Error> {
    if !value.is_boolean() {
        return Err(agent_client_protocol::Error::invalid_params().data("expected a boolean value"));
    }
    Ok(())
}

fn validate_unit_volume(value: &serde_json::Value) -> Result<(), agent_client_protocol::Error> {
    let Some(v) = value.as_f64() else {
        return Err(agent_client_protocol::Error::invalid_params()
            .data("soundNotificationVolume must be a number"));
    };
    if !v.is_finite() || !(0.0..=1.0).contains(&v) {
        return Err(agent_client_protocol::Error::invalid_params()
            .data("soundNotificationVolume must be in [0.0, 1.0]"));
    }
    Ok(())
}

/// Accepts `{ widgetToggle, indicatorStop, appQuit }` JSON. Each value
/// must be a non-empty string (a Tauri accelerator). Missing entries
/// are allowed (the caller falls back to defaults), but unknown keys
/// are rejected so a typo doesn't silently round-trip into config.
fn validate_keyboard_global_shortcuts(
    value: &serde_json::Value,
) -> Result<(), agent_client_protocol::Error> {
    let Some(obj) = value.as_object() else {
        return Err(agent_client_protocol::Error::invalid_params()
            .data("keyboardGlobalShortcuts must be a JSON object"));
    };
    const ALLOWED: &[&str] = &["widgetToggle", "indicatorStop", "appQuit"];
    for (k, v) in obj {
        if !ALLOWED.contains(&k.as_str()) {
            return Err(agent_client_protocol::Error::invalid_params()
                .data(format!("keyboardGlobalShortcuts: unknown action '{k}'")));
        }
        let Some(s) = v.as_str() else {
            return Err(agent_client_protocol::Error::invalid_params()
                .data(format!("keyboardGlobalShortcuts.{k} must be a string")));
        };
        if s.trim().is_empty() {
            return Err(agent_client_protocol::Error::invalid_params()
                .data(format!("keyboardGlobalShortcuts.{k} must be non-empty")));
        }
    }
    Ok(())
}

// ── /RUSKY FORK PATCH: sound + keyboard preferences validators (G8) ───────

fn is_supported_voice_dictation_provider(value: &str) -> bool {
    matches!(value, "openai" | "groq" | "elevenlabs" | "__disabled__") || {
        #[cfg(feature = "local-inference")]
        {
            value == "local"
        }
        #[cfg(not(feature = "local-inference"))]
        {
            false
        }
    }
}

fn optional_config_string(
    config: &Config,
    key: &str,
) -> Result<Option<String>, agent_client_protocol::Error> {
    match config.get_param::<String>(key) {
        Ok(value) => Ok(Some(value)),
        Err(crate::config::ConfigError::NotFound(_)) => Ok(None),
        Err(e) => Err(agent_client_protocol::Error::internal_error().data(e.to_string())),
    }
}
