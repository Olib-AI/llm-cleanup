//! Format adapters. Each implements [`cleanup_core::Format`]: parse bytes into prose spans,
//! and render edits by splicing into the original bytes (never by reserializing).

pub mod c2pa;
pub mod docx;
pub mod html;
pub mod image;
pub mod markdown;
pub mod sniff;
pub mod text;

pub use docx::Docx;
pub use html::Html;
pub use image::ImageFormat;
pub use markdown::Markdown;
pub use sniff::{format_for_path, formatter_for_path};
pub use text::PlainText;
