use std::collections::HashMap;
use std::sync::Arc;

use lattice_runtime::Runtime;

pub(crate) fn model_runtime(credentials: HashMap<String, String>) -> Runtime {
    Runtime::builder()
        .credentials(credentials)
        .session_store(Arc::new(crate::session::session_manager()))
        .build()
}
