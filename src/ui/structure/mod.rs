//! UI modals, confirmation builders, sidebar refresh helpers, and dispatchers for
//! structure management (projects, sub-projects, sections, labels; EPIC-09).

#![allow(dead_code, unused_imports)]

pub mod confirm;
pub mod dispatch;
pub mod form;
pub mod sidebar;

pub use confirm::{build_confirm_dialog, StructureConfirmAction};
pub use dispatch::{
    dispatch_confirm, dispatch_intent, error_to_toast, execute_confirm,
    prompt_archive_selected_project, prompt_for_action, submit_form, StructurePrompt,
};
pub use form::{StructureForm, StructureFormEvent, StructureFormKind, StructureIntent};
pub use sidebar::refresh_after_structure_change;
