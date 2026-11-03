#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicDeviceSnapshot {
    pub id: usize,
    pub name: String,
    pub is_default: bool,
    pub platform_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicSelection {
    pub device_id: Option<usize>,
    pub key: String,
    pub label: String,
    pub setting: String,
    pub fallback: bool,
}

pub fn resolve_mic_selection(saved: Option<&str>, devices: &[MicDeviceSnapshot]) -> MicSelection {
    let saved = saved
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("__default__");
    let device_id = selected_device_id(saved, devices);
    let selected = device_id.and_then(|id| devices.iter().find(|d| d.id == id));
    let default = devices.iter().find(|d| d.is_default);
    let fallback = !matches!(saved, "__default__")
        && !saved.eq_ignore_ascii_case("default")
        && device_id.is_none();
    let (key, resolved_label) = match selected.or(default) {
        Some(d) if device_id.is_some() => (device_setting(d), d.name.clone()),
        Some(d) => (
            format!("default:{}", d.name),
            format!("System default ({})", d.name),
        ),
        None => (
            "default:<unknown>".to_string(),
            "System default".to_string(),
        ),
    };
    let label = if fallback {
        saved_device_name(saved).unwrap_or_else(|| "Saved microphone".to_string())
    } else {
        resolved_label
    };
    MicSelection {
        device_id,
        key,
        label,
        setting: saved.to_string(),
        fallback,
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
struct SavedMicDevice {
    platform_id: Option<String>,
    name: String,
}

fn device_setting(device: &MicDeviceSnapshot) -> String {
    let saved = SavedMicDevice {
        platform_id: device.platform_id.clone(),
        name: device.name.clone(),
    };
    format!(
        "device:{}",
        serde_json::to_string(&saved).expect("serialize microphone selection")
    )
}

fn saved_device_name(saved: &str) -> Option<String> {
    if let Some(serialized) = saved.strip_prefix("device:") {
        return serde_json::from_str::<SavedMicDevice>(serialized)
            .ok()
            .map(|device| device.name);
    }
    if let Some(rest) = saved.strip_prefix("id:") {
        return rest.split_once(':').map(|(_, name)| name.to_string());
    }
    (!saved.is_empty()).then(|| saved.to_string())
}

fn selected_device_id(saved: &str, devices: &[MicDeviceSnapshot]) -> Option<usize> {
    if saved == "__default__" || saved.eq_ignore_ascii_case("default") {
        return None;
    }
    if let Some(serialized) = saved.strip_prefix("device:") {
        if let Ok(saved_device) = serde_json::from_str::<SavedMicDevice>(serialized) {
            if let Some(platform_id) = saved_device.platform_id.as_deref() {
                if let Some(device) = devices
                    .iter()
                    .find(|device| device.platform_id.as_deref() == Some(platform_id))
                {
                    return Some(device.id);
                }
            }
            return unique_device_id_by_name(&saved_device.name, devices);
        }
    }
    if let Some(rest) = saved.strip_prefix("id:") {
        let mut parts = rest.splitn(2, ':');
        if let Some(id) = parts.next().and_then(|s| s.parse::<usize>().ok()) {
            let expected_name = parts.next();
            if devices
                .iter()
                .any(|d| d.id == id && expected_name.map(|name| d.name == name).unwrap_or(true))
            {
                return Some(id);
            }
            if let Some(name) = expected_name {
                return unique_device_id_by_name(name, devices);
            }
        }
    }
    if let Ok(id) = saved.parse::<usize>() {
        if devices.iter().any(|d| d.id == id) {
            return Some(id);
        }
    }
    unique_device_id_by_name(saved, devices)
}

fn unique_device_id_by_name(name: &str, devices: &[MicDeviceSnapshot]) -> Option<usize> {
    let mut matches = devices.iter().filter(|device| device.name == name);
    let first = matches.next()?;
    matches.next().is_none().then_some(first.id)
}
