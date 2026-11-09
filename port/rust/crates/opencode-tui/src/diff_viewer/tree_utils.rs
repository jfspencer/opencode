//! `feature-plugins/system/diff-viewer-file-tree-utils.ts`: the changed files as a directory tree (single-child
//! directory chains collapsed into one row), its rows, and the file order and moves the viewer uses.
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};

/// `FileTreeItem`.
#[derive(Clone, Debug, PartialEq)]
pub struct FileTreeItem {
    pub file: String,
    pub status: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Directory,
    File,
}

/// `FileTreeNode`.
#[derive(Clone, Debug, PartialEq)]
pub struct FileTreeNode {
    pub id: usize,
    pub name: String,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub depth: usize,
    pub kind: NodeKind,
    pub file_index: Option<usize>,
}

/// `FileTree`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileTree {
    pub roots: Vec<usize>,
    pub nodes: Vec<FileTreeNode>,
}

/// `FileTreeRow`.
#[derive(Clone, Debug, PartialEq)]
pub struct FileTreeRow {
    pub id: usize,
    pub depth: usize,
    pub kind: NodeKind,
    pub name: String,
    pub file_index: Option<usize>,
}

fn add_node(nodes: &mut Vec<FileTreeNode>, roots: &mut Vec<usize>, name: &str, parent: Option<usize>, depth: usize, kind: NodeKind, file_index: Option<usize>) -> usize {
    let id = nodes.len();
    nodes.push(FileTreeNode { id, name: name.to_string(), parent, children: Vec::new(), depth, kind, file_index });
    match parent {
        None => roots.push(id),
        Some(parent) => nodes[parent].children.push(id),
    }
    id
}

/// JS `<` on strings: UTF-16 code units.
fn js_cmp(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// `compareFileTreeNodes(tree, left, right)`: directories first, then by name, then by id.
pub fn compare_nodes(tree: &FileTree, left: usize, right: usize) -> Ordering {
    let (l, r) = (&tree.nodes[left], &tree.nodes[right]);
    if l.kind != r.kind {
        return if l.kind == NodeKind::Directory { Ordering::Less } else { Ordering::Greater };
    }
    js_cmp(&l.name, &r.name).then(left.cmp(&right))
}

/// `buildFileTree(files)`.
pub fn build_file_tree(files: &[FileTreeItem]) -> FileTree {
    let mut roots = Vec::new();
    let mut nodes = Vec::new();
    let mut directories: HashMap<String, usize> = HashMap::new();
    for (file_index, file) in files.iter().enumerate() {
        let segments: Vec<&str> = file.file.split('/').filter(|s| !s.is_empty()).collect();
        if segments.is_empty() {
            continue;
        }
        let mut parent: Option<usize> = None;
        let mut path = String::new();
        let mut depth = 0;
        for segment in &segments[..segments.len() - 1] {
            let directory = if path.is_empty() { segment.to_string() } else { format!("{path}/{segment}") };
            let id = match directories.get(&directory) {
                Some(id) => *id,
                None => {
                    let id = add_node(&mut nodes, &mut roots, segment, parent, depth, NodeKind::Directory, None);
                    directories.insert(directory.clone(), id);
                    id
                }
            };
            parent = Some(id);
            path = directory;
            depth += 1;
        }
        add_node(&mut nodes, &mut roots, segments[segments.len() - 1], parent, depth, NodeKind::File, Some(file_index));
    }
    let mut tree = FileTree { roots, nodes };
    let snapshot = tree.clone();
    tree.roots.sort_by(|a, b| compare_nodes(&snapshot, *a, *b));
    for node in tree.nodes.iter_mut() {
        node.children.sort_by(|a, b| compare_nodes(&snapshot, *a, *b));
    }
    tree
}

/// `collapsedFileTreeDirectoryChain(tree, id)`.
fn collapsed_chain(tree: &FileTree, id: usize) -> Vec<&FileTreeNode> {
    let node = &tree.nodes[id];
    let child = if node.children.len() == 1 { Some(&tree.nodes[node.children[0]]) } else { None };
    match child {
        Some(child) if child.kind == NodeKind::Directory => {
            let mut chain = vec![node];
            chain.extend(collapsed_chain(tree, child.id));
            chain
        }
        _ => vec![node],
    }
}

/// `flattenFileTree(tree, expanded)`: every row when `expanded` is `None`.
pub fn flatten_file_tree(tree: &FileTree, expanded: Option<&BTreeSet<usize>>) -> Vec<FileTreeRow> {
    fn visit(tree: &FileTree, expanded: Option<&BTreeSet<usize>>, id: usize, depth: usize, rows: &mut Vec<FileTreeRow>) {
        let node = &tree.nodes[id];
        if node.kind == NodeKind::File {
            rows.push(FileTreeRow { id: node.id, depth, kind: node.kind, name: node.name.clone(), file_index: node.file_index });
            return;
        }
        let chain = collapsed_chain(tree, node.id);
        let last = chain[chain.len() - 1];
        rows.push(FileTreeRow {
            id: node.id,
            depth,
            kind: node.kind,
            name: chain.iter().map(|n| n.name.as_str()).collect::<Vec<_>>().join("/"),
            file_index: node.file_index,
        });
        if expanded.is_none_or(|e| e.contains(&node.id)) {
            for child in last.children.clone() {
                visit(tree, expanded, child, depth + 1, rows);
            }
        }
    }
    let mut rows = Vec::new();
    for root in &tree.roots {
        visit(tree, expanded, *root, 0, &mut rows);
    }
    rows
}

/// `fileTreeFileSelection(tree, fileIndex)`: the directories to expand to show the file.
pub fn file_tree_file_selection(tree: &FileTree, file_index: usize) -> Option<BTreeSet<usize>> {
    let node = tree.nodes.iter().find(|n| n.kind == NodeKind::File && n.file_index == Some(file_index))?;
    let mut result = BTreeSet::new();
    let mut parent = node.parent;
    while let Some(p) = parent {
        result.insert(p);
        parent = tree.nodes.get(p).and_then(|n| n.parent);
    }
    Some(result)
}

/// `singlePatchFileIndex(selected, current, first)`.
pub fn single_patch_file_index(selected: Option<usize>, current: Option<usize>, first: Option<usize>) -> Option<usize> {
    selected.or(current).or(first)
}

/// `orderedPatchFileIndexes(rows)`.
pub fn ordered_patch_file_indexes(rows: &[FileTreeRow]) -> Vec<usize> {
    rows.iter().filter_map(|r| r.file_index).collect()
}

/// `showDiffViewerFileTree(showFileTree, fileCount)`.
pub fn show_diff_viewer_file_tree(show: bool, count: usize) -> bool {
    show && count > 0
}

/// `movePatchFileIndex(fileIndexes, current, offset)`.
pub fn move_patch_file_index(indexes: &[usize], current: Option<usize>, offset: i64) -> Option<usize> {
    if indexes.is_empty() {
        return None;
    }
    let Some(index) = current.and_then(|c| indexes.iter().position(|i| *i == c)) else { return Some(indexes[0]) };
    let next = (index as i64 + offset).clamp(0, indexes.len() as i64 - 1) as usize;
    Some(indexes[next])
}

/// `allExpandedFileTreeDirectories(tree)`.
pub fn all_expanded_directories(tree: &FileTree) -> BTreeSet<usize> {
    tree.nodes.iter().filter(|n| n.kind == NodeKind::Directory).map(|n| n.id).collect()
}

/// `toggleFileTreeDirectory(tree, expanded, selected)`.
pub fn toggle_directory(tree: &FileTree, expanded: &BTreeSet<usize>, selected: Option<usize>) -> BTreeSet<usize> {
    let Some(selected) = selected.filter(|s| tree.nodes.get(*s).is_some_and(|n| n.kind == NodeKind::Directory)) else {
        return expanded.clone();
    };
    let mut next = expanded.clone();
    if !next.remove(&selected) {
        next.insert(selected);
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree() {
        let files: Vec<FileTreeItem> = ["src/math.ts", "src/new.ts", "src/old.ts", "README.md", "a/b/c/d.ts"]
            .iter()
            .map(|f| FileTreeItem { file: f.to_string(), status: None })
            .collect();
        let tree = build_file_tree(&files);
        let rows = flatten_file_tree(&tree, None);
        let names: Vec<(usize, &str)> = rows.iter().map(|r| (r.depth, r.name.as_str())).collect();
        assert_eq!(names, vec![(0, "a/b/c"), (1, "d.ts"), (0, "src"), (1, "math.ts"), (1, "new.ts"), (1, "old.ts"), (0, "README.md")]);
        assert_eq!(ordered_patch_file_indexes(&rows), vec![4, 0, 1, 2, 3]);
        assert_eq!(move_patch_file_index(&[4, 0, 1], Some(0), 1), Some(1));
        assert_eq!(move_patch_file_index(&[4, 0, 1], Some(1), 1), Some(1));
    }
}
