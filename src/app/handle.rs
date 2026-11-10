use super::{records::ToggleOverrides, store::AppSessionStore, view::SessionView};
use std::{ops::Deref, sync::Arc};
use tokio::sync::{mpsc, oneshot, watch};
use vva_core::{
    harness::{AgentEvent, InputMode, ManagedSession as CoreHandle},
    model::ContentPart,
};
pub struct ManagedSession {
    projected: Arc<std::sync::Mutex<watch::Sender<Arc<SessionView>>>>,
    record: Arc<watch::Sender<super::records::SessionRecord>>,
    pub id: String,
    pub core: Arc<CoreHandle>,
    pub view: watch::Receiver<Arc<SessionView>>,
    store: Arc<AppSessionStore>,
}
impl Deref for ManagedSession {
    type Target = CoreHandle;
    fn deref(&self) -> &Self::Target {
        &self.core
    }
}
impl ManagedSession {
    pub fn new(core: Arc<CoreHandle>, store: Arc<AppSessionStore>) -> Result<Arc<Self>, String> {
        let record = store.slot(&core.id).map_err(|e| e.to_string())?;
        let mut records = record.subscribe();
        let mut views = core.subscribe();
        let initial = SessionView::project((**views.borrow()).clone(), &records.borrow(), None);
        let (tx, view) = watch::channel(Arc::new(initial));
        let projected = Arc::new(std::sync::Mutex::new(tx));
        let changes = projected.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {result=views.changed()=>{if result.is_err(){break;}},result=records.changed()=>{if result.is_err(){break;}},}
                let tx = changes.lock().expect("view projection lock");
                let previous = tx.borrow().clone();
                let core = views.borrow().clone();
                let record = records.borrow();
                if previous.core_revision != core.revision
                    || previous.record.as_ref() != Some(&record)
                {
                    let projected = SessionView::project((*core).clone(), &record, Some(&previous));
                    tx.send_replace(Arc::new(projected));
                }
            }
        });
        Ok(Arc::new(Self {
            id: core.id.clone(),
            core,
            view,
            store,
            projected,
            record,
        }))
    }
    pub fn snapshot(&self) -> Arc<SessionView> {
        let tx = self.projected.lock().expect("view projection lock");
        let previous = tx.borrow().clone();
        let core = self.core.snapshot();
        let record = self.record.borrow();
        if previous.core_revision == core.revision && previous.record.as_ref() == Some(&record) {
            return previous;
        }
        let view = Arc::new(SessionView::project(
            (*core).clone(),
            &record,
            Some(&previous),
        ));
        tx.send_replace(view.clone());
        view
    }
    pub fn subscribe(&self) -> watch::Receiver<Arc<SessionView>> {
        self.view.clone()
    }
    pub async fn dismiss_notice(&self, id: String) -> Result<(), String> {
        {
            let tx = self.projected.lock().expect("view projection lock");
            let mut view = (**tx.borrow()).clone();
            if view
                .application_notices
                .iter()
                .any(|notice| notice.id == id)
            {
                view.application_notices.retain(|notice| notice.id != id);
                view.notices.retain(|notice| notice.id != id);
                view.revision += 1;
                tx.send_replace(Arc::new(view));
                return Ok(());
            }
        }
        self.core.dismiss_notice(id).await
    }
    pub async fn configure(
        &self,
        model: Option<String>,
        variant: Option<Option<String>>,
        toggles: ToggleOverrides,
    ) -> Result<(), String> {
        self.core.configure(model, variant).await?;
        if let Some(enabled) = toggles.web_search {
            self.store.update_record(&self.id, |record| {
                let changed = record.coding.web_search != enabled;
                record.coding.web_search = enabled;
                Ok(changed)
            })?;
            self.core.configure(None, None).await?;
        }
        Ok(())
    }
    pub async fn submit(
        &self,
        prompt: &str,
        attachments: Vec<ContentPart>,
        events: Option<mpsc::Sender<AgentEvent>>,
    ) -> Result<(), String> {
        let seq = self.core.turn_seq();
        self.core
            .submit(prompt, attachments, events.clone())
            .await?;
        let handle = self.core.clone();
        let store = self.store.clone();
        let id = self.id.clone();
        let projected = self.projected.clone();
        tokio::spawn(async move {
            let result = match handle.settled_report(seq).await {
                Ok(_) => crate::space::activity::touch(&store, &id),
                Err(error) => Err(error),
            };
            if let Err(error) = result {
                {
                    let tx = projected.lock().expect("view projection lock");
                    let mut view = (**tx.borrow()).clone();
                    let notice = vva_core::harness::Notice::new(
                        error.clone(),
                        vva_core::harness::NoticeKind::Session,
                    );
                    view.application_notices.push(notice.clone());
                    view.notices.push(notice);
                    view.revision += 1;
                    tx.send_replace(Arc::new(view));
                }
                if let Some(events) = events {
                    let _ = events.send(AgentEvent::Error(error)).await;
                }
            }
        });
        Ok(())
    }
    pub async fn deliver(
        &self,
        text: &str,
    ) -> Result<vva_core::harness::session::handle::DeliveryReceipt, String> {
        self.deliver_input(text, InputMode::After).await
    }
    async fn deliver_input(
        &self,
        text: &str,
        mode: InputMode,
    ) -> Result<vva_core::harness::session::handle::DeliveryReceipt, String> {
        let receipt = self.core.deliver_input(text, mode).await?;
        let (tx, rx) = oneshot::channel();
        let store = self.store.clone();
        let id = self.id.clone();
        tokio::spawn(async move {
            let result = match receipt.await {
                Ok(result) => result,
                Err(error) => Err(error.to_string()),
            };
            let result = match crate::space::activity::touch(&store, &id) {
                Ok(()) => result,
                Err(error) => Err(error),
            };
            let _ = tx.send(result);
        });
        Ok(rx)
    }
    async fn wait_delivery(
        receipt: vva_core::harness::session::handle::DeliveryReceipt,
    ) -> Result<vva_core::tools::agent::SubagentReport, String> {
        let report = receipt.await.map_err(|e| e.to_string())??;
        match report.error {
            Some(error) => Err(error),
            None => Ok(report),
        }
    }
    pub async fn deliver_wait(
        &self,
        text: &str,
    ) -> Result<vva_core::tools::agent::SubagentReport, String> {
        Self::wait_delivery(self.deliver(text).await?).await
    }
    pub async fn steer_wait(
        &self,
        text: &str,
    ) -> Result<vva_core::tools::agent::SubagentReport, String> {
        Self::wait_delivery(self.deliver_input(text, InputMode::Steer).await?).await
    }
}
