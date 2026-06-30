use crate::core::error::ToolkitError;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct XmlEvent {
    pub id: u32,
    pub name: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct XmlSoundbank {
    pub id: u32,
    pub name: String,
    pub events: Vec<XmlEvent>,
}

/// Parses events and metadata from Wwise's SoundBanksInfo.xml file.
pub fn parse_soundbanks_info_xml(xml: &str) -> Result<Vec<XmlSoundbank>, ToolkitError> {
    let mut soundbanks = Vec::new();
    let mut current_bank: Option<XmlSoundbank> = None;
    let mut in_included_events = false;

    let mut cursor = 0;
    while let Some(tag_start) = xml[cursor..].find('<') {
        let abs_start = cursor + tag_start;
        let rest = &xml[abs_start..];
        let tag_end = match rest.find('>') {
            Some(idx) => idx,
            None => break,
        };
        let tag_content = rest[1..tag_end].trim();
        cursor = abs_start + tag_end + 1;

        if tag_content.starts_with("SoundBank ") {
            let id = extract_attr(tag_content, "Id")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            current_bank = Some(XmlSoundbank {
                id,
                name: String::new(),
                events: Vec::new(),
            });
            in_included_events = false;
        } else if tag_content == "IncludedEvents" {
            in_included_events = true;
        } else if tag_content == "/IncludedEvents" {
            in_included_events = false;
        } else if tag_content.starts_with("Event ") && in_included_events {
            if let Some(ref mut bank) = current_bank {
                let id = extract_attr(tag_content, "Id")
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0);
                let name = extract_attr(tag_content, "Name")
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                if id != 0 && !name.is_empty() {
                    bank.events.push(XmlEvent { id, name });
                }
            }
        } else if tag_content == "/SoundBank" {
            if let Some(bank) = current_bank.take() {
                soundbanks.push(bank);
            }
        } else if tag_content.starts_with("ShortName") {
            if let Some(close_tag_idx) = xml[cursor..].find("</ShortName>") {
                let name = xml[cursor..cursor + close_tag_idx].trim().to_string();
                if let Some(ref mut bank) = current_bank {
                    bank.name = name;
                }
                cursor += close_tag_idx + "</ShortName>".len();
            }
        }
    }

    Ok(soundbanks)
}

fn extract_attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let search = format!("{}=\"", name);
    let start_idx = tag.find(&search)?;
    let val_start = start_idx + search.len();
    let rest = &tag[val_start..];
    let end_idx = rest.find('"')?;
    Some(&rest[..end_idx])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_xml() {
        let xml_data = r#"<?xml version="1.0" encoding="utf-8"?>
<SoundBanksInfo Platform="Windows" BasePlatform="Windows" SchemaVersion="12" SoundBankVersion="135">
  <SoundBanks>
    <SoundBank Id="123456" Type="User" Language="SFX" Hash="999">
      <ShortName>wpn_sheepinator</ShortName>
      <Path>wpn_sheepinator.bnk</Path>
      <IncludedEvents>
        <Event Id="98765" Name="play_sheepinator" ObjectPath="\Events\DefaultWorkUnit\play_sheepinator"/>
        <Event Id="43210" Name="stop_sheepinator" ObjectPath="\Events\DefaultWorkUnit\stop_sheepinator"/>
      </IncludedEvents>
    </SoundBank>
  </SoundBanks>
</SoundBanksInfo>"#;

        let result = parse_soundbanks_info_xml(xml_data).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, 123456);
        assert_eq!(result[0].name, "wpn_sheepinator");
        assert_eq!(result[0].events.len(), 2);
        assert_eq!(result[0].events[0].id, 98765);
        assert_eq!(result[0].events[0].name, "play_sheepinator");
    }
}
