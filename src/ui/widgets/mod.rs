//! Modal & interactive TUI widgets (`T-704`..`T-711`, EPIC-08).

#![allow(dead_code, unused_imports)]

pub mod comments;
pub mod confirm;
pub mod errors_view;
pub mod picker;
pub mod task_form;

pub use comments::{CommentsEvent, CommentsModal};
pub use confirm::{ConfirmDialog, ConfirmEvent};
pub use errors_view::{ErrorToast, ErrorsEvent, ErrorsModal};
pub use picker::{DestinationPicker, DestinationTarget, PickerEntry, PickerEvent};
pub use task_form::{FormEvent, FormField, FormMode, TaskForm, TaskFormValues};
