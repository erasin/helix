use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use anyhow::Result;
use helix_core::movement::Direction;
use helix_view::{
    graphics::Rect,
    icons::ICONS,
    input::{Event, KeyEvent, MouseButton, MouseEvent, MouseEventKind},
    theme::Style,
};

use crate::{
    compositor::{Component, Context, EventResult},
    ctrl, key, shift,
    ui::{self, Prompt},
};
use tui::{
    buffer::Buffer as Surface,
    text::{Span, Spans},
};

pub trait TreeViewItem: Sized + Ord {
    type Params: Default;

    fn name(&self) -> String;
    fn path(&self) -> PathBuf;
    fn is_parent(&self) -> bool;

    fn filter(&self, s: &str) -> bool {
        self.name().to_lowercase().contains(&s.to_lowercase())
    }

    fn get_children(&self) -> Result<Vec<Self>>;
}

fn tree_item_cmp<T: TreeViewItem>(item1: &T, item2: &T) -> Ordering {
    T::cmp(item1, item2)
}

fn vec_to_tree<T: TreeViewItem>(mut items: Vec<T>) -> Vec<Tree<T>> {
    items.sort();
    index_elems(
        0,
        items
            .into_iter()
            .map(|item| Tree::new(item, vec![]))
            .collect(),
    )
}

pub enum TreeOp {
    Noop,
    GetChildsAndInsert,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Tree<T> {
    item: T,
    parent_index: Option<usize>,
    index: usize,
    children: Vec<Self>,

    /// Why do we need this property?
    /// Can't we just use `!children.is_empty()`?
    ///
    /// Because we might have for example an open folder that is empty,
    /// and user just added a new file under that folder,
    /// and the user refreshes the whole tree.
    ///
    /// Without `open`, we will not refresh any node without children,
    /// and thus the folder still appears empty after refreshing.
    is_opened: bool,
}

impl<T: Clone> Clone for Tree<T> {
    fn clone(&self) -> Self {
        Self {
            item: self.item.clone(),
            index: self.index,
            children: self.children.clone(),
            is_opened: self.is_opened,
            parent_index: self.parent_index,
        }
    }
}

#[derive(Clone)]
struct TreeIter<'a, T> {
    current_index_forward: usize,
    current_index_reverse: isize,
    tree: &'a Tree<T>,
}

impl<'a, T> Iterator for TreeIter<'a, T> {
    type Item = &'a Tree<T>;

    fn next(&mut self) -> Option<Self::Item> {
        let index = self.current_index_forward;
        if index > self.tree.len().saturating_sub(1) {
            None
        } else {
            self.current_index_forward = self.current_index_forward.saturating_add(1);
            self.tree.get(index)
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.tree.len(), Some(self.tree.len()))
    }
}

impl<'a, T> DoubleEndedIterator for TreeIter<'a, T> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let index = self.current_index_reverse;
        if index < 0 {
            None
        } else {
            self.current_index_reverse = self.current_index_reverse.saturating_sub(1);
            self.tree.get(index as usize)
        }
    }
}

impl<'a, T> ExactSizeIterator for TreeIter<'a, T> {}

impl<T: TreeViewItem> Tree<T> {
    fn open(&mut self) -> Result<()> {
        if self.item.is_parent() {
            self.children = self.get_children()?;
            self.is_opened = true;
        }
        Ok(())
    }

    fn close(&mut self) {
        self.is_opened = false;
        self.children = vec![];
    }

    fn refresh(&mut self) -> Result<()> {
        if !self.is_opened {
            return Ok(());
        }
        let latest_children = self.get_children()?;
        let mut latest_map: HashMap<String, Tree<T>> = latest_children
            .into_iter()
            .map(|child| (child.item.name(), child))
            .collect();

        let mut kept = Vec::new();
        for mut tree in std::mem::take(&mut self.children) {
            if latest_map.remove(&tree.item.name()).is_some() {
                tree.refresh()?;
                kept.push(tree);
            }
        }

        let new_nodes = latest_map.into_values();
        self.children = kept.into_iter().chain(new_nodes).collect();

        self.sort();

        self.regenerate_index();

        Ok(())
    }

    fn get_children(&self) -> Result<Vec<Tree<T>>> {
        Ok(vec_to_tree(self.item.get_children()?))
    }

    fn sort(&mut self) {
        self.children
            .sort_by(|a, b| tree_item_cmp(&a.item, &b.item))
    }
}

impl<T> Tree<T> {
    pub fn new(item: T, children: Vec<Tree<T>>) -> Self {
        let is_opened = !children.is_empty();
        Self {
            item,
            index: 0,
            parent_index: None,
            children: index_elems(0, children),
            is_opened,
        }
    }

    fn iter(&self) -> TreeIter<'_, T> {
        TreeIter {
            tree: self,
            current_index_forward: 0,
            current_index_reverse: (self.len() - 1) as isize,
        }
    }

    /// Find an element in the tree with given `predicate`.
    /// `start_index` is inclusive if direction is `Forward`.
    /// `start_index` is exclusive if direction is `Backward`.
    fn find<F>(&self, start_index: usize, direction: Direction, mut predicate: F) -> Option<usize>
    where
        F: FnMut(&Tree<T>) -> bool,
    {
        match direction {
            Direction::Forward => match self
                .iter()
                .skip(start_index)
                .position(&mut predicate)
                .map(|index| index + start_index)
            {
                Some(index) => Some(index),
                None => self.iter().position(&mut predicate),
            },

            Direction::Backward => match self.iter().take(start_index).rposition(&mut predicate) {
                Some(index) => Some(index),
                None => self.iter().rposition(predicate),
            },
        }
    }

    pub fn item(&self) -> &T {
        &self.item
    }

    fn get(&self, index: usize) -> Option<&Tree<T>> {
        if self.index == index {
            Some(self)
        } else {
            self.children.iter().find_map(|elem| elem.get(index))
        }
    }

    fn get_mut(&mut self, index: usize) -> Option<&mut Tree<T>> {
        if self.index == index {
            Some(self)
        } else {
            self.children
                .iter_mut()
                .find_map(|elem| elem.get_mut(index))
        }
    }

    fn len(&self) -> usize {
        (1_usize).saturating_add(self.children.iter().map(|elem| elem.len()).sum())
    }

    fn regenerate_index(&mut self) {
        let items = std::mem::take(&mut self.children);
        self.children = index_elems(0, items);
    }
}

#[derive(Clone, Debug)]
struct SavedView {
    selected: usize,
    winline: usize,
}

pub struct TreeView<T: TreeViewItem> {
    tree: Tree<T>,

    search_prompt: Option<(Direction, Prompt)>,

    search_str: String,

    /// Selected item idex
    selected: usize,

    backward_jumps: Vec<usize>,
    forward_jumps: Vec<usize>,

    saved_view: Option<SavedView>,

    /// For implementing vertical scroll
    winline: usize,

    /// For implementing horizontal scoll
    column: usize,

    tree_len: usize,

    /// For implementing horizontal scoll
    max_len: usize,
    count: usize,
    #[allow(clippy::type_complexity)]
    pre_render: Option<Box<dyn Fn(&mut Self, Rect) + 'static>>,

    #[allow(clippy::type_complexity)]
    on_opened_fn: Option<Box<dyn FnMut(&mut T, &mut Context, &mut T::Params) -> TreeOp + 'static>>,

    #[allow(clippy::type_complexity)]
    on_folded_fn: Option<Box<dyn FnMut(&mut T, &mut Context, &mut T::Params) + 'static>>,

    #[allow(clippy::type_complexity)]
    on_next_key: Option<Box<dyn FnMut(&mut Context, &mut Self, &KeyEvent) -> Result<()>>>,
}

impl<T: TreeViewItem> TreeView<T> {
    pub fn build_tree(root: T) -> Result<Self> {
        let children = root.get_children()?;
        let items = vec_to_tree(children);
        let tree = Tree::new(root, items);
        let tree_len = tree.len();
        Ok(Self {
            tree_len,
            tree,
            selected: 0,
            backward_jumps: vec![],
            forward_jumps: vec![],
            saved_view: None,
            winline: 0,
            column: 0,
            max_len: 0,
            count: 0,
            pre_render: None,
            on_opened_fn: None,
            on_folded_fn: None,
            on_next_key: None,
            search_prompt: None,
            search_str: "".into(),
        })
    }

    pub fn with_enter_fn<F>(mut self, f: F) -> Self
    where
        F: FnMut(&mut T, &mut Context, &mut T::Params) -> TreeOp + 'static,
    {
        self.on_opened_fn = Some(Box::new(f));
        self
    }

    #[allow(dead_code)]
    pub fn with_folded_fn<F>(mut self, f: F) -> Self
    where
        F: FnMut(&mut T, &mut Context, &mut T::Params) + 'static,
    {
        self.on_folded_fn = Some(Box::new(f));
        self
    }

    /// Reveal item in the tree based on the given `segments`.
    ///
    /// The name of the root should be excluded.
    ///
    /// Example `segments`:
    ///
    ///    vec!["helix-term", "src", "ui", "tree.rs"]
    ///
    pub fn reveal_item(&mut self, segments: Vec<String>) -> Result<()> {
        // Expand the tree
        let root = self.tree.item.path().display().to_string();
        segments.iter().try_fold(
            &mut self.tree,
            |current_tree, segment| {
                    match current_tree
                        .children
                        .iter_mut()
                        .find(|tree| tree.item.name().eq(segment))
                    {
                        Some(tree) => {
                            if !tree.is_opened {
                                tree.open()?;
                            }
                            Ok(tree)
                        }
                        None => Err(anyhow::anyhow!(format!(
                            "Unable to find path: '{}'. current_segment = '{segment}'. current_root = '{root}'",
                            segments.join("/"),
                        ))),
                    }
            },
        )?;

        // Locate the item
        self.regenerate_index();
        self.set_selected(
            segments
                .iter()
                .fold(&self.tree, |tree, segment| {
                    tree.children
                        .iter()
                        .find(|tree| tree.item.name().eq(segment))
                        .expect("Should be unreachable")
                })
                .index,
        );

        self.align_view_center();
        Ok(())
    }

    fn align_view_center(&mut self) {
        self.pre_render = Some(Box::new(|tree, area| {
            tree.winline = area.height as usize / 2
        }))
    }

    fn align_view_top(&mut self) {
        self.winline = 0
    }

    fn align_view_bottom(&mut self) {
        self.pre_render = Some(Box::new(|tree, area| tree.winline = area.height as usize))
    }

    fn regenerate_index(&mut self) {
        self.tree.regenerate_index();
        self.tree_len = self.tree.len();
    }

    fn move_to_parent(&mut self) -> Result<()> {
        if let Some(parent) = self.current_parent()? {
            let index = parent.index;
            self.set_selected(index)
        }
        Ok(())
    }

    fn move_to_children(&mut self) -> Result<()> {
        let current = self.current_mut()?;
        if current.is_opened {
            self.set_selected(self.selected + 1);
            Ok(())
        } else {
            current.open()?;
            if !current.children.is_empty() {
                self.set_selected(self.selected + 1);
                self.regenerate_index();
            }
            Ok(())
        }
    }

    pub fn refresh(&mut self) -> Result<()> {
        self.tree.refresh()?;
        self.set_selected(self.selected);
        Ok(())
    }

    fn move_to_first_line(&mut self) {
        self.move_up(usize::MAX / 2)
    }

    fn move_to_last_line(&mut self) {
        self.move_down(usize::MAX / 2)
    }

    fn move_leftmost(&mut self) {
        self.move_left(usize::MAX / 2);
    }

    fn move_rightmost(&mut self) {
        self.move_right(usize::MAX / 2)
    }

    fn restore_saved_view(&mut self) -> Result<()> {
        if let Some(saved_view) = self.saved_view.take() {
            self.selected = saved_view.selected;
            self.winline = saved_view.winline;
            self.refresh()
        } else {
            Ok(())
        }
    }

    pub fn prompt(&self) -> Option<&Prompt> {
        if let Some((_, prompt)) = self.search_prompt.as_ref() {
            Some(prompt)
        } else {
            None
        }
    }

    pub fn handle_mouse_event(
        &mut self,
        event: &MouseEvent,
        cxt: &mut Context,
        params: &mut T::Params,
    ) -> EventResult {
        let MouseEvent { kind, row, .. } = *event;

        match kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let cow = row as isize - self.winline as isize;
                let selected = if cow > 0 {
                    self.selected.saturating_add(cow as usize)
                } else {
                    self.selected.saturating_sub(cow.unsigned_abs())
                };

                if self.selected == selected {
                    self.on_enter(cxt, params, self.selected)
                        .unwrap_or_default();
                } else {
                    self.set_selected(selected);
                }
                EventResult::Consumed(None)
            }
            MouseEventKind::ScrollUp => {
                self.pre_render = Some(Box::new(|tree, area| {
                    // 滚动处理,selected - wineline 来检测是否到了顶部
                    if tree.selected.saturating_sub(tree.winline) > 3 {
                        if area.height as usize - 3 > tree.winline {
                            tree.winline = tree.winline.saturating_add(1);
                        } else {
                            tree.move_up(1);
                        }
                    }
                }));
                EventResult::Consumed(None)
            }
            MouseEventKind::ScrollDown => {
                self.pre_render = Some(Box::new(|tree, _area| {
                    if tree.winline > 3 {
                        tree.winline = tree.winline.saturating_sub(1);
                    } else {
                        tree.move_down(1);
                    }
                }));
                EventResult::Consumed(None)
            }
            _ => EventResult::Ignored(None),
        }
    }
}

pub fn tree_view_help() -> Vec<(&'static str, &'static str)> {
    vec![
        ("o, Enter", "Open/Close"),
        ("j, down, C-n", "Down"),
        ("k, up, C-p", "Up"),
        ("h, left", "Go to parent"),
        ("l, right", "Expand"),
        ("J", "Go to next sibling"),
        ("K", "Go to previous sibling"),
        ("H", "Go to first child"),
        ("L", "Go to last child"),
        ("R", "Refresh"),
        ("/", "Search"),
        ("n", "Go to next search match"),
        ("N", "Go to previous search match"),
        ("gh, Home", "Scroll to the leftmost"),
        ("gl, End", "Scroll to the rightmost"),
        ("C-o", "Jump backward"),
        ("C-i, Tab", "Jump forward"),
        ("C-d", "Half page down"),
        ("C-u", "Half page up"),
        ("PageDown", "Full page down"),
        ("PageUp", "Full page up"),
        ("zt", "Align view top"),
        ("zz", "Align view center"),
        ("zb", "Align view bottom"),
        ("gg", "Go to first line"),
        ("ge", "Go to last line"),
    ]
}

impl<T: TreeViewItem> TreeView<T> {
    pub fn on_enter(
        &mut self,
        cx: &mut Context,
        params: &mut T::Params,
        selected_index: usize,
    ) -> Result<()> {
        let selected_item = self.get_mut(selected_index)?;
        if selected_item.is_opened {
            selected_item.close();
            self.regenerate_index();
            return Ok(());
        }

        if let Some(mut on_open_fn) = self.on_opened_fn.take() {
            let mut f = || -> Result<()> {
                let current = self.current_mut()?;
                match on_open_fn(&mut current.item, cx, params) {
                    TreeOp::GetChildsAndInsert => {
                        if let Err(err) = current.open() {
                            cx.editor.set_error(format!("{err}"))
                        }
                    }
                    TreeOp::Noop => {}
                };
                Ok(())
            };
            f()?;
            self.regenerate_index();
            self.on_opened_fn = Some(on_open_fn);
        };
        Ok(())
    }

    fn set_search_str(&mut self, s: String) {
        self.search_str = s;
        self.saved_view = None;
    }

    fn saved_view(&self) -> SavedView {
        self.saved_view.clone().unwrap_or(SavedView {
            selected: self.selected,
            winline: self.winline,
        })
    }

    fn search_next(&mut self, s: &str) {
        let saved_view = self.saved_view();
        let skip = std::cmp::max(2, saved_view.selected + 1);
        self.set_selected(
            self.tree
                .find(skip, Direction::Forward, |e| e.item.filter(s))
                .unwrap_or(saved_view.selected),
        );
    }

    fn search_previous(&mut self, s: &str) {
        let saved_view = self.saved_view();
        let take = saved_view.selected;
        self.set_selected(
            self.tree
                .find(take, Direction::Backward, |e| e.item.filter(s))
                .unwrap_or(saved_view.selected),
        );
    }

    fn move_to_next_search_match(&mut self) {
        self.search_next(&self.search_str.clone())
    }

    fn move_to_previous_next_match(&mut self) {
        self.search_previous(&self.search_str.clone())
    }

    pub fn move_down(&mut self, rows: usize) {
        self.set_selected(self.selected.saturating_add(rows))
    }

    fn set_selected(&mut self, selected: usize) {
        let previous_selected = self.selected;
        self.set_selected_without_history(selected);
        if previous_selected.abs_diff(selected) > 1 {
            self.backward_jumps.push(previous_selected)
        }
    }

    fn set_selected_without_history(&mut self, selected: usize) {
        let selected = selected.clamp(0, self.tree_len.saturating_sub(1));
        if selected > self.selected {
            // Move down
            self.winline = selected.min(
                self.winline
                    .saturating_add(selected.saturating_sub(self.selected)),
            );
        } else {
            // Move up
            self.winline = selected.min(
                self.winline
                    .saturating_sub(self.selected.saturating_sub(selected)),
            );
        }
        self.selected = selected
    }

    fn jump_backward(&mut self) {
        if let Some(index) = self.backward_jumps.pop() {
            self.forward_jumps.push(self.selected);
            self.set_selected_without_history(index);
        }
    }

    fn jump_forward(&mut self) {
        if let Some(index) = self.forward_jumps.pop() {
            self.set_selected(index)
        }
    }

    pub fn move_up(&mut self, rows: usize) {
        self.set_selected(self.selected.saturating_sub(rows))
    }

    fn move_to_next_sibling(&mut self) -> Result<()> {
        if let Some(parent) = self.current_parent()? {
            if let Some(local_index) = parent
                .children
                .iter()
                .position(|child| child.index == self.selected)
            {
                if let Some(next_sibling) = parent.children.get(local_index.saturating_add(1)) {
                    self.set_selected(next_sibling.index)
                }
            }
        }
        Ok(())
    }

    fn move_to_previous_sibling(&mut self) -> Result<()> {
        if let Some(parent) = self.current_parent()? {
            if let Some(local_index) = parent
                .children
                .iter()
                .position(|child| child.index == self.selected)
            {
                if let Some(next_sibling) = parent.children.get(local_index.saturating_sub(1)) {
                    self.set_selected(next_sibling.index)
                }
            }
        }
        Ok(())
    }

    fn move_to_last_sibling(&mut self) -> Result<()> {
        if let Some(parent) = self.current_parent()? {
            if let Some(last) = parent.children.last() {
                self.set_selected(last.index)
            }
        }
        Ok(())
    }

    fn move_to_first_sibling(&mut self) -> Result<()> {
        if let Some(parent) = self.current_parent()? {
            if let Some(last) = parent.children.first() {
                self.set_selected(last.index)
            }
        }
        Ok(())
    }

    fn move_left(&mut self, cols: usize) {
        self.column = self.column.saturating_sub(cols);
    }

    fn move_right(&mut self, cols: usize) {
        self.pre_render = Some(Box::new(move |tree, area| {
            let max_scroll = tree
                .max_len
                .saturating_sub(area.width as usize)
                .saturating_add(1);
            tree.column = max_scroll.min(tree.column + cols);
        }));
    }

    fn move_down_half_page(&mut self) {
        self.pre_render = Some(Box::new(|tree, area| {
            tree.move_down((area.height / 2) as usize);
        }));
    }

    fn move_up_half_page(&mut self) {
        self.pre_render = Some(Box::new(|tree, area| {
            tree.move_up((area.height / 2) as usize);
        }));
    }

    fn move_down_page(&mut self) {
        self.pre_render = Some(Box::new(|tree, area| {
            tree.move_down((area.height) as usize);
        }));
    }

    fn move_up_page(&mut self) {
        self.pre_render = Some(Box::new(|tree, area| {
            tree.move_up((area.height) as usize);
        }));
    }

    fn save_view(&mut self) {
        self.saved_view = Some(SavedView {
            selected: self.selected,
            winline: self.winline,
        })
    }

    fn get(&self, index: usize) -> Result<&Tree<T>> {
        self.tree.get(index).ok_or_else(|| {
            anyhow::anyhow!("Programming error: TreeView.get: index {index} is out of bound")
        })
    }

    fn get_mut(&mut self, index: usize) -> Result<&mut Tree<T>> {
        self.tree.get_mut(index).ok_or_else(|| {
            anyhow::anyhow!("Programming error: TreeView.get_mut: index {index} is out of bound")
        })
    }

    pub fn current(&self) -> Result<&Tree<T>> {
        self.get(self.selected)
    }

    pub fn current_mut(&mut self) -> Result<&mut Tree<T>> {
        self.get_mut(self.selected)
    }

    fn current_parent(&self) -> Result<Option<&Tree<T>>> {
        if let Some(parent_index) = self.current()?.parent_index {
            Ok(Some(self.get(parent_index)?))
        } else {
            Ok(None)
        }
    }

    pub fn current_item(&self) -> Result<&T> {
        Ok(&self.current()?.item)
    }
}

#[derive(Clone)]
struct RenderedLine<'a> {
    indent: Spans<'a>,
    content: Span<'a>,
    selected: bool,
    is_ancestor_of_current_item: bool,
}
struct RenderTreeParams<'a, T> {
    tree: &'a Tree<T>,
    prefix: Spans<'a>,
    level: usize,
    selected: usize,
    selected_style: Style,
    text_style: Style,
    directory_style: Style,
    indent_guide_style: Style,
    indent_guide_char: char,
}

fn render_tree<'a, T: TreeViewItem>(
    params: RenderTreeParams<'a, T>,
    ancestor_indices: &HashSet<usize>,
    vcs_styles: &HashMap<PathBuf, Style>,
) -> Vec<RenderedLine<'a>> {
    let RenderTreeParams {
        tree,
        prefix,
        level,
        selected,
        selected_style,
        text_style,
        directory_style,
        indent_guide_style,
        indent_guide_char,
    } = params;
    let is_selected = selected == tree.index;
    let is_ancestor_of_current_item = ancestor_indices.contains(&tree.index);

    let style = if is_selected {
        selected_style
    } else {
        // Use VCS diff style if available, otherwise default text style
        vcs_styles
            .get(&tree.item.path())
            .copied()
            .unwrap_or(text_style)
    };

    let ancestor_style = if is_ancestor_of_current_item {
        directory_style
    } else {
        style
    };

    let mut indent = prefix.clone();
    let mut prefix = prefix.clone();
    if level > 0 {
        // TODO: ICON V2
        let indicator = if tree.item().is_parent() {
            if tree.is_opened {
                Span::styled(" ", style)
            } else {
                Span::styled(" ", style)
            }
        } else {
            let icons = ICONS.load();
            if let Some(icon) = icons.fs().from_path(&tree.item.path()) {
                let icon_color = if is_selected { None } else { icon.color() };
                if let Some(color) = icon_color {
                    Span::styled(format!("{} ", icon.glyph()), Style::default().fg(color))
                } else {
                    Span::raw(format!("{} ", icon.glyph()))
                }
            } else {
                Span::styled("  ", style)
            }
        };

        indent.0.push(indicator);
        prefix.0.push(Span::styled(
            indent_guide_char.to_string(),
            indent_guide_style,
        ));
        prefix.0.push(Span::raw(" "));
    }

    let name = Span::styled(tree.item.name(), ancestor_style);
    let head = RenderedLine {
        indent,
        selected: is_selected,
        is_ancestor_of_current_item,
        content: name,
    };

    vec![head]
        .into_iter()
        .chain(tree.children.iter().flat_map(|elem| {
            render_tree(
                RenderTreeParams {
                    tree: elem,
                    prefix: prefix.clone(),
                    level: level + 1,
                    selected,
                    selected_style,
                    text_style,
                    directory_style,
                    indent_guide_style,
                    indent_guide_char,
                },
                ancestor_indices,
                vcs_styles,
            )
        }))
        .collect()
}

impl<T: TreeViewItem + Clone> TreeView<T> {
    pub fn render(
        &mut self,
        area: Rect,
        prompt_area: Rect,
        surface: &mut Surface,
        cx: &mut Context,
        vcs_style_map: &HashMap<PathBuf, Style>,
    ) {
        if let Some((_, prompt)) = self.search_prompt.as_mut() {
            prompt.render_prompt(prompt_area, surface, cx)
        }

        self.render_lines(area, cx, vcs_style_map)
            .into_iter()
            .enumerate()
            .for_each(|(index, line)| {
                let area = Rect {
                    y: area.y.saturating_add(index as u16),
                    height: 1,
                    ..area
                };

                if line.selected {
                    surface.clear_with(area, line.content.style);
                }

                let indent_len = line.indent.width() as u16;
                surface.set_spans(area.x, area.y, &line.indent, indent_len);
                let x = area.x.saturating_add(indent_len);
                surface.set_span(
                    x,
                    area.y,
                    &line.content,
                    area.width.saturating_sub(indent_len).saturating_sub(1),
                );
            });
    }

    fn render_lines(
        &mut self,
        area: Rect,
        cx: &mut Context,
        vcs_style_map: &HashMap<PathBuf, Style>,
    ) -> Vec<RenderedLine<'_>> {
        if let Some(pre_render) = self.pre_render.take() {
            pre_render(self, area);
        }

        self.winline = self.winline.min(area.height.saturating_sub(1) as usize);
        let skip = self.selected.saturating_sub(self.winline);

        let selected_style = cx.editor.theme.get("ui.menu.selected");
        let text_style = cx.editor.theme.get("ui.text");
        let directory_style = cx.editor.theme.get("ui.text.directory");
        let indent_guide_style = cx.editor.theme.get("ui.virtual.indent-guide");
        let indent_guide_char = cx.editor.config.load().indent_guides.character;

        let params = RenderTreeParams {
            tree: &self.tree,
            prefix: Span::raw(" ").into(),
            level: 0,
            selected: self.selected,
            selected_style,
            text_style,
            directory_style,
            indent_guide_style,
            indent_guide_char,
        };

        let mut ancestor_indices = HashSet::new();
        let mut current_idx = self.selected;
        while let Some(current) = self.tree.get(current_idx) {
            if let Some(parent_idx) = current.parent_index {
                ancestor_indices.insert(parent_idx);
                current_idx = parent_idx;
            } else {
                break;
            }
        }

        let lines = render_tree(params, &ancestor_indices, vcs_style_map);

        self.max_len = lines
            .iter()
            .map(|line| line.indent.width())
            .max()
            .unwrap_or(0);

        let take = area.height as usize;

        struct RetainAncestorResult<'a> {
            skipped_ancestors: Vec<RenderedLine<'a>>,
            remaining_lines: Vec<RenderedLine<'a>>,
        }
        fn retain_ancestors(lines: Vec<RenderedLine>, skip: usize) -> RetainAncestorResult {
            let mut remaining = lines;
            let mut all_skipped_ancestors = Vec::new();
            let mut skip = skip;

            loop {
                if skip == 0 || remaining.is_empty() {
                    break;
                }
                if remaining.first().is_some_and(|line| line.selected) {
                    break;
                }

                let selected_index = remaining.iter().position(|line| line.selected);
                skip = match selected_index {
                    None => skip,
                    Some(selected_index) => skip.min(selected_index),
                };

                let split_point = skip.min(remaining.len().saturating_sub(1));
                let skipped: Vec<_> = remaining.drain(..split_point).collect();

                let ancestor_count = skipped
                    .iter()
                    .filter(|line| line.is_ancestor_of_current_item)
                    .count();
                all_skipped_ancestors.extend(
                    skipped
                        .into_iter()
                        .filter(|line| line.is_ancestor_of_current_item),
                );

                skip = ancestor_count;
            }

            RetainAncestorResult {
                skipped_ancestors: all_skipped_ancestors,
                remaining_lines: remaining,
            }
        }

        let RetainAncestorResult {
            skipped_ancestors,
            remaining_lines,
        } = retain_ancestors(lines, skip);

        let max_ancestors_len = take.saturating_sub(1);

        // Skip furthest ancestors
        let skipped_ancestors = skipped_ancestors
            .into_iter()
            .rev()
            .take(max_ancestors_len)
            .rev()
            .collect::<Vec<_>>();

        let skipped_ancestors_len = skipped_ancestors.len();

        skipped_ancestors
            .into_iter()
            .chain(
                remaining_lines
                    .into_iter()
                    .take(take.saturating_sub(skipped_ancestors_len)),
            )
            // Horizontal scroll
            .map(|line| RenderedLine {
                indent: if line.indent.0.is_empty() {
                    Spans::default()
                } else {
                    line.indent
                },
                content: line.content,
                ..line
            })
            .collect()
    }

    pub fn handle_key_event(
        &mut self,
        event: &Event,
        cx: &mut Context,
        params: &mut T::Params,
    ) -> EventResult {
        let key_event = match event {
            Event::Key(event) => event,
            Event::Resize(..) => return EventResult::Consumed(None),
            _ => return EventResult::Ignored(None),
        };
        (|| -> Result<EventResult> {
            if let Some(mut on_next_key) = self.on_next_key.take() {
                on_next_key(cx, self, key_event)?;
                return Ok(EventResult::Consumed(None));
            }

            if let EventResult::Consumed(c) = self.handle_search_event(key_event, cx) {
                return Ok(EventResult::Consumed(c));
            }

            let count = std::mem::replace(&mut self.count, 0);

            match key_event {
                key!(i @ '0'..='9') => {
                    self.count = i.to_digit(10).unwrap_or(0) as usize + count * 10
                }
                shift!('J') => self.move_to_next_sibling()?,
                shift!('K') => self.move_to_previous_sibling()?,
                shift!('H') => self.move_to_first_sibling()?,
                shift!('L') => self.move_to_last_sibling()?,
                key!('j') | key!(Down) | ctrl!('n') => self.move_down(1.max(count)),
                key!('k') | key!(Up) | ctrl!('p') => self.move_up(1.max(count)),
                key!('h') | key!(Left) => self.move_to_parent()?,
                key!('l') | key!(Right) => self.move_to_children()?,
                key!(Enter) | key!('o') => self.on_enter(cx, params, self.selected)?,
                ctrl!('d') => self.move_down_half_page(),
                ctrl!('u') => self.move_up_half_page(),
                key!('z') => {
                    self.on_next_key = Some(Box::new(|_, tree, event| {
                        match event {
                            key!('z') => tree.align_view_center(),
                            key!('t') => tree.align_view_top(),
                            key!('b') => tree.align_view_bottom(),
                            _ => {}
                        };
                        Ok(())
                    }));
                }
                key!('g') => {
                    self.on_next_key = Some(Box::new(|_, tree, event| {
                        match event {
                            key!('g') => tree.move_to_first_line(),
                            key!('e') => tree.move_to_last_line(),
                            key!('h') => tree.move_leftmost(),
                            key!('l') => tree.move_rightmost(),
                            _ => {}
                        };
                        Ok(())
                    }));
                }
                key!('/') => self.new_search_prompt(Direction::Forward),
                key!('n') => self.move_to_next_search_match(),
                shift!('N') => self.move_to_previous_next_match(),
                key!(PageDown) => self.move_down_page(),
                key!(PageUp) => self.move_up_page(),
                shift!('R') => {
                    if let Err(error) = self.refresh() {
                        cx.editor.set_error(error.to_string())
                    }
                }
                key!(Home) => self.move_leftmost(),
                key!(End) => self.move_rightmost(),
                ctrl!('o') => self.jump_backward(),
                ctrl!('i') | key!(Tab) => self.jump_forward(),
                _ => return Ok(EventResult::Ignored(None)),
            };
            Ok(EventResult::Consumed(None))
        })()
        .unwrap_or_else(|err| {
            cx.editor.set_error(format!("{err}"));
            EventResult::Consumed(None)
        })
    }

    fn handle_search_event(&mut self, event: &KeyEvent, cx: &mut Context) -> EventResult {
        if let Some((direction, mut prompt)) = self.search_prompt.take() {
            match event {
                key!(Enter) => {
                    self.set_search_str(prompt.line().clone());
                    EventResult::Consumed(None)
                }
                key!(Esc) => {
                    if let Err(err) = self.restore_saved_view() {
                        cx.editor.set_error(format!("{err}"))
                    }
                    EventResult::Consumed(None)
                }
                _ => {
                    let event = prompt.handle_event(&Event::Key(*event), cx);
                    let line = prompt.line();
                    match direction {
                        Direction::Forward => {
                            self.search_next(line);
                        }
                        Direction::Backward => self.search_previous(line),
                    }
                    self.search_prompt = Some((direction, prompt));
                    event
                }
            }
        } else {
            EventResult::Ignored(None)
        }
    }

    fn new_search_prompt(&mut self, direction: Direction) {
        self.save_view();
        self.search_prompt = Some((
            direction,
            Prompt::new("search: ".into(), None, ui::completers::none, |_, _, _| {}),
        ))
    }

    pub fn prompting(&self) -> bool {
        self.search_prompt.is_some() || self.on_next_key.is_some()
    }
}

/// Recalculate the index of each item of a tree.
///
/// For example:
///
/// ```txt
/// foo (0)
///   bar (1)
/// spam (2)
///   jar (3)
///     yo (4)
/// ```
fn index_elems<T>(parent_index: usize, elems: Vec<Tree<T>>) -> Vec<Tree<T>> {
    fn index_elems<T>(
        current_index: usize,
        elems: Vec<Tree<T>>,
        parent_index: usize,
    ) -> (usize, Vec<Tree<T>>) {
        elems
            .into_iter()
            .fold((current_index, vec![]), |(current_index, trees), elem| {
                let index = current_index;
                let item = elem.item;
                let (current_index, folded) = index_elems(current_index + 1, elem.children, index);
                let tree = Tree {
                    item,
                    children: folded,
                    index,
                    is_opened: elem.is_opened,
                    parent_index: Some(parent_index),
                };
                (current_index, trees.into_iter().chain(vec![tree]).collect())
            })
    }
    index_elems(parent_index + 1, elems, parent_index).1
}
