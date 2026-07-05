pub mod files_view;
pub mod largest_view;
pub mod sunburst_view;
pub mod tree_view;
pub mod treemap_view;

#[derive(Clone, Copy)]
pub enum ContextAction {
    OpenInFinder,
    RevealInFinder,
    CopyPath,
    Delete,
}
