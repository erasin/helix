use std::path::PathBuf;

use helix_view::graphics::Rect;

use super::tree::TreeView;

#[derive(Clone, Debug, Default)]
pub struct State {
    pub focus: bool,
    pub open: bool,
    pub current_root: PathBuf,
    pub area_width: u16,
    pub area: Rect,
}

impl State {
    pub fn new(focus: bool, current_root: PathBuf) -> Self {
        Self {
            focus,
            current_root,
            open: true,
            area_width: 0,
            area: Rect::default(),
        }
    }
}

// FileInfo is defined in the parent module (explorer.rs).
use super::FileInfo;

pub struct ExplorerHistory {
    pub tree: TreeView<FileInfo>,
    pub current_root: PathBuf,
}
