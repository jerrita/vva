//! OPENAI_API_KEY=... OPENAI_MODEL=... VVA_SESSION_DIR=/path/to/sessions
//! cargo run -p vva-core --example headless --features response,jsonl -- 'Uppercase hello using the tool.'
use std::{env, sync::Arc};

use async_trait::async_trait;
use serde::Deserialize;
use vva_core::{
    harness::{AgentAssembly, ExecutionPlan, FixedSetup, SessionManager},
    model::{Message, Role, Session, ToolInput, Turn},
    provider::{CompletionOptions, InputCapabilities, response::ResponseProvider},
    storage::jsonl::JsonlSessionStore,
    tools::{Tool, ToolRegistry, parse_args},
};

struct Uppercase;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    text: String,
}

#[async_trait]
impl Tool for Uppercase {
    fn name(&self) -> &str {
        "uppercase"
    }
    fn description(&self) -> &str {
        "Convert text to uppercase."
    }
    fn input(&self) -> ToolInput {
        ToolInput::Json(serde_json::json!({
            "type": "object", "properties": {"text": {"type": "string"}},
            "required": ["text"], "additionalProperties": false
        }))
    }
    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let arguments: Arguments = parse_args(arguments, self.name())?;
        Ok(arguments.text.to_uppercase())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let key = env::var("OPENAI_API_KEY")?;
    let model = env::var("OPENAI_MODEL")?;
    let directory = env::var_os("VVA_SESSION_DIR").ok_or("VVA_SESSION_DIR is required")?;
    let prompt = env::args()
        .nth(1)
        .ok_or("Pass a prompt as the first argument")?;
    let store = Arc::new(JsonlSessionStore::new(directory)?);
    let manager = Arc::new(SessionManager::new(store));
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(Uppercase));
    let setup = Arc::new(FixedSetup::new(ExecutionPlan {
        provider: Arc::new(ResponseProvider::new(key).with_websocket(false)),
        options: CompletionOptions {
            model: model.clone(),
            ..Default::default()
        },
        inputs: InputCapabilities {
            model: model.clone(),
            modalities: vec!["text".into()],
            ..Default::default()
        },
        tools,
        notices: Vec::new(),
        external_web: Vec::new(),
    }));
    let mut initial = Session::with_model(env::current_dir()?, None, model, None);
    initial.append_turn(Turn::new(
        Role::System,
        vec![Message::text(
            "Use the uppercase tool when asked to convert text.",
        )],
    ));
    let session = manager
        .create(AgentAssembly {
            session: initial,
            setup,
        })
        .await?;
    let report = session.deliver_wait(&prompt).await;
    // Always reclaim tasks, including when the provider or tool fails.
    manager.shutdown().await?;
    println!("{}", report?.text);
    Ok(())
}
