//! ネイティブダイアログの薄いラッパ（rfd）。ロジックは `agentdir` にあり、ここは `Ui` を実装するだけ。
use crate::agentdir::Ui;
use std::path::{Path, PathBuf};

pub struct NativeUi;

impl Ui for NativeUi {
    fn pick_folder(&mut self, title: &str, start_dir: Option<&Path>) -> Option<PathBuf> {
        let mut d = rfd::FileDialog::new().set_title(title);
        if let Some(s) = start_dir {
            d = d.set_directory(s);
        }
        d.pick_folder()
    }

    fn error(&mut self, title: &str, msg: &str) {
        message_box(title, msg);
    }
}

/// 致命エラー用のメッセージボックス（OK のみ）。
pub fn message_box(title: &str, msg: &str) {
    let _ = rfd::MessageDialog::new().set_level(rfd::MessageLevel::Error).set_title(title).set_description(msg).set_buttons(rfd::MessageButtons::Ok).show();
}
