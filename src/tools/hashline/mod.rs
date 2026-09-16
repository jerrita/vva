//! omp hashline: file snapshot tags, original line numbers, PUT/CUT/REM/MV.

mod apply;
mod block;
mod parse;
mod session;
mod snapshot;
#[cfg(test)]
mod tests;

use super::Tool;
use super::filesystem::parse_args;
use async_trait::async_trait;
use parse::parse_patch;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

use apply::apply_sections;
pub use session::{HashlineSession, file_tag};
pub use snapshot::{format_grep_file, format_read};

pub struct HashlineEditTool {
    working_dir: PathBuf,
    session: Option<Arc<HashlineSession>>,
}

impl HashlineEditTool {
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
            session: None,
        }
    }

    pub fn with_session(mut self, session: Arc<HashlineSession>) -> Self {
        self.session = Some(session);
        self
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HashlineArgs {
    input: String,
}

#[async_trait]
impl Tool for HashlineEditTool {
    fn name(&self) -> &str {
        "Edit"
    }

    fn description(&self) -> &str {
        r#"Line-anchored patch. Copy [PATH#TAG] from Read, Grep, Write, or the latest Edit preview; numbers are original snapshot lines, not shifted by earlier hunks. New files use Write.

{"input":"[src/example.rs#1A2B]\nPUT 4.=4:\n+const value = 2;\n"}

PUT N.=M: replace inclusive N–M with + body; every line in the range is removed, so include any retained lines in the body. Prefer <N or >N for insertion.
PUT N*: replace a heuristically detected block (bracket counting, indentation, or Markdown heading; no language parser). Use explicit ranges when strings, comments, or JSX affect boundaries.
PUT <N: insert before N (`PUT <1:` = file head). PUT >N: after N. PUT >$: append. PUT >N*: after block N.
CUT N.=M / CUT N* deletes and optionally captures @name. PUT <N @name / PUT >N @name / PUT N.=M @name pastes (range/block paste needs a named register; gap paste may be anonymous).
REM deletes the section file. MV DEST moves after prior edits in that section.
Body rows start with +; + alone writes a blank line. Literal leading - or +: +-text / ++text. Never send -old or context rows.
After Edit, use its new [PATH#TAG]; displayed lines outside changed ranges carry over with shifted numbers, so only genuinely unseen ranges need a Read. All hunks in one request use the original snapshot's numbers. Read/Grep, Write, and numbered Edit previews accumulate displayed lines for the same TAG. Elided and partial lines are unseen. Active-session snapshots survive submissions; after session reload, Read/Grep again."#
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "input": {
                    "type": "string",
                    "description": "One or more [PATH#TAG] sections of PUT/CUT/REM/MV. Optional *** Begin Patch / *** End Patch wrapper."
                }
            },
            "required": ["input"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: HashlineArgs = parse_args(arguments, "Edit")?;
        let sections = parse_patch(&args.input)?;
        let working_dir = self.working_dir.clone();
        let session = self
            .session
            .clone()
            .ok_or_else(|| "hashline session is missing; Read the file before Edit".to_string())?;
        tokio::task::spawn_blocking(move || apply_sections(&working_dir, &session, sections))
            .await
            .map_err(|e| format!("Edit task failed: {e}"))?
    }
}
