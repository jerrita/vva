use crate::model::{ContentPart, MediaKind, MediaSource, Session};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ProviderType {
    #[default]
    Response,
    Anthropic,
    Gemini,
}

impl ProviderType {
    pub fn require_media(
        self,
        kind: crate::model::MediaKind,
        mime_type: &str,
    ) -> Result<(), String> {
        use crate::model::MediaKind;
        let supported = match (self, kind) {
            (Self::Gemini, _) => true,
            (_, MediaKind::Pdf) => mime_type == "application/pdf",
            (_, MediaKind::Image) => matches!(
                mime_type,
                "image/png" | "image/jpeg" | "image/gif" | "image/webp"
            ),
            _ => false,
        };
        if supported {
            Ok(())
        } else {
            Err(format!(
                "Protocol '{}' does not support {mime_type} input",
                self.as_str()
            ))
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Response => "response",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct InputCapabilities {
    pub model: String,
    pub modalities: Vec<String>,
    pub protocol: ProviderType,
}

impl InputCapabilities {
    pub fn require(&self, kind: MediaKind, mime_type: &str) -> Result<(), String> {
        if !self.modalities.iter().any(|input| input == kind.as_str()) {
            return Err(format!(
                "Model '{}' does not support {} input",
                self.model,
                kind.as_str()
            ));
        }
        self.protocol.require_media(kind, mime_type)
    }

    pub fn validate_session(&self, session: &Session) -> Result<(), String> {
        for part in session
            .turns
            .iter()
            .flat_map(|turn| &turn.messages)
            .flat_map(|msg| &msg.parts)
        {
            self.validate_part(part)?;
        }
        Ok(())
    }
    pub fn validate_part(&self, part: &ContentPart) -> Result<(), String> {
        match part {
            ContentPart::Image { source, mime_type } => {
                if !self.modalities.iter().any(|input| input == "image") {
                    return Err(format!(
                        "Model '{}' does not support image input",
                        self.model
                    ));
                }
                let mime = match source {
                    MediaSource::Base64 { mime_type, .. } => Some(mime_type.as_str()),
                    MediaSource::Url { .. } => mime_type.as_deref(),
                };
                if let Some(mime) = mime {
                    self.protocol.require_media(MediaKind::Image, mime)?;
                }
            }
            ContentPart::File {
                source: MediaSource::Base64 { mime_type, .. },
                mime_type: declared_mime,
                ..
            } => {
                if declared_mime != mime_type {
                    return Err("File MIME type does not match its source".into());
                }
                self.require(MediaKind::Pdf, mime_type)?;
            }
            ContentPart::ToolResult(result) => {
                for media in &result.attachments {
                    self.require(media.kind, &media.mime_type)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}
