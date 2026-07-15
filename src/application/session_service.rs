use std::sync::Arc;

use uuid::Uuid;

use unitprep_core::csv_document::CsvDocument;
use unitprep_core::parsing::parse_document;
use unitprep_core::session_store::SessionStore;
use unitprep_core::uploaded_file::UploadedFile;

use crate::domain::session::Session;

pub struct SessionService {
    store: Arc<dyn SessionStore<Session>>,
}

impl SessionService {
    pub fn new(
        store: Arc<dyn SessionStore<Session>>,
    ) -> Self {
        Self { store }
    }

    pub fn create_session(
        &self,
        uploaded_files: Vec<UploadedFile>,
    ) -> String {
        tracing::info!(
            uploaded_files = uploaded_files.len(),
            "Creating session"
        );

        let session_id =
            Uuid::new_v4().to_string();

        let mut session =
            Session::new(session_id.clone());

        let mut documents: Vec<CsvDocument> =
            Vec::new();

        let mut skipped = 0usize;

        for file in &uploaded_files {
            match parse_document(file) {
                Ok(doc) => {
                    documents.push(doc);
                }

                Err(err) => {
                    skipped += 1;

                    tracing::warn!(
                        file = %file.file_name,
                        error = %err,
                        "Skipping file — could not parse document"
                    );
                }
            }
        }

        tracing::info!(
            csv_documents = documents.len(),
            files_received = uploaded_files.len(),
            files_skipped = skipped,
            "Session contents before save"
        );

        session.data.documents =
            Arc::new(documents);

        self.store.save(session);

        tracing::info!(
            session_id = %session_id,
            "Session created"
        );

        session_id
    }
}
