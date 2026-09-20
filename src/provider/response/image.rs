use crate::model::{ImageGenerationCall, ImageGenerationStatus, MediaSource};
use crate::provider::ProviderError;
/// 流式条目和最终响应共用同一领域投影。
pub(super) fn project_image_generation_call(
    id: &str,
    status: &str,
    result: Option<&str>,
    output_format: Option<&str>,
    revised_prompt: Option<&str>,
) -> Result<ImageGenerationCall, ProviderError> {
    let status = match status {
        "in_progress" => ImageGenerationStatus::InProgress,
        "completed" => ImageGenerationStatus::Completed,
        "incomplete" => ImageGenerationStatus::Incomplete,
        _ => {
            return Err(ProviderError::Protocol(format!(
                "Invalid image generation status: {status}"
            )));
        }
    };
    Ok(ImageGenerationCall {
        id: id.to_string(),
        status,
        // 官方规范：result 为 base64 图像；output_format 取值 png/jpeg/webp，缺省 png
        result: result.map(|data| MediaSource::Base64 {
            data: data.to_string(),
            mime_type: match output_format {
                Some("jpeg") => "image/jpeg",
                Some("webp") => "image/webp",
                _ => "image/png",
            }
            .into(),
        }),
        revised_prompt: revised_prompt.map(str::to_string),
    })
}
