import { useState, useCallback, memo } from "react";
import styles from "./AssetBrowser.module.css";

export interface AssetSpan {
  span: number;
  size: number;
  archiveIndex: number;
}

export interface TreeNodeData {
  name: string;
  fullPath: string;
  children: Map<string, TreeNodeData>;
  asset?: { id: string; spans: AssetSpan[] };
}

interface TreeViewProps {
  root: TreeNodeData;
  selectedPaths: Set<string>;
  onSelect: (node: TreeNodeData, event: React.MouseEvent) => void;
  onContextMenu?: (node: TreeNodeData, event: React.MouseEvent) => void;
  filter: string;
  depth?: number;
}

const TreeNode = memo(function TreeNode({
  node,
  selectedPaths,
  onSelect,
  onContextMenu,
  filter,
  depth,
}: {
  node: TreeNodeData;
  selectedPaths: Set<string>;
  onSelect: (node: TreeNodeData, event: React.MouseEvent) => void;
  onContextMenu?: (node: TreeNodeData, event: React.MouseEvent) => void;
  filter: string;
  depth: number;
}) {
  const isFolder = node.children.size > 0;
  const [expanded, setExpanded] = useState(false);
  const forceExpand = filter.length > 0;

  const handleClick = useCallback((event: React.MouseEvent) => {
    if (isFolder) {
      setExpanded((prev) => !prev);
    } else {
      onSelect(node, event);
    }
  }, [isFolder, node, onSelect]);

  const handleContextMenu = useCallback((event: React.MouseEvent) => {
    if (!isFolder && onContextMenu) {
      event.preventDefault();
      onSelect(node, event);
      onContextMenu(node, event);
    }
  }, [isFolder, node, onSelect, onContextMenu]);

  const isOpen = forceExpand || expanded;
  const isSelected = !isFolder && selectedPaths.has(node.fullPath);

  const sortedChildren = Array.from(node.children.values()).sort((a, b) => {
    const aIsFolder = a.children.size > 0;
    const bIsFolder = b.children.size > 0;
    if (aIsFolder !== bIsFolder) return aIsFolder ? -1 : 1;
    return a.name.localeCompare(b.name);
  });

  const filteredChildren = filter
    ? sortedChildren.filter((child) => matchesFilter(child, filter))
    : sortedChildren;

  if (filter && !isFolder && !node.fullPath.toLowerCase().includes(filter)) {
    return null;
  }

  return (
    <div className={styles.treeNode}>
      <div
        className={`${styles.treeRow} ${isSelected ? styles.selected : ""}`}
        style={{ paddingLeft: `${depth * 16 + 4}px` }}
        onClick={handleClick}
        onContextMenu={handleContextMenu}
        title={node.asset ? `ID: ${node.asset.id}${node.asset.spans.length > 1 ? ` · ${node.asset.spans.length} spans` : ""}` : node.fullPath}
      >
        {isFolder ? (
          <>
            <span className={styles.folderToggle}>{isOpen ? "▾" : "▸"}</span>
            <span className={styles.folderIcon}>📁</span>
          </>
        ) : (
          <>
            <span className={styles.folderToggle} />
            <span className={styles.fileIcon}>·</span>
          </>
        )}
        <span className={styles.nodeName}>{node.name}</span>
      </div>

      {isFolder && isOpen && (
        <div className={styles.children}>
          {filteredChildren.map((child) => (
            <TreeNode
              key={child.fullPath}
              node={child}
              selectedPaths={selectedPaths}
              onSelect={onSelect}
              onContextMenu={onContextMenu}
              filter={filter}
              depth={depth + 1}
            />
          ))}
        </div>
      )}
    </div>
  );
});

function matchesFilter(node: TreeNodeData, filter: string): boolean {
  if (node.fullPath.toLowerCase().includes(filter)) return true;
  for (const child of node.children.values()) {
    if (matchesFilter(child, filter)) return true;
  }
  return false;
}

export default function TreeView({ root, selectedPaths, onSelect, onContextMenu, filter, depth = 0 }: TreeViewProps) {
  const lowerFilter = filter.toLowerCase();

  // Flat filtered results when searching
  if (lowerFilter) {
    const results: TreeNodeData[] = [];
    const filterTerms = lowerFilter.split(/\s+/).filter(Boolean);

    function search(node: TreeNodeData) {
      if (results.length >= 200) return;
      if (node.asset) {
        const pathLower = node.fullPath.toLowerCase();
        if (filterTerms.every(term => pathLower.includes(term))) {
          results.push(node);
        }
      }
      for (const child of node.children.values()) {
        if (results.length >= 200) return;
        search(child);
      }
    }

    for (const child of root.children.values()) {
      search(child);
    }

    return (
      <div style={{ marginLeft: "4px" }}>
        {results.map((node) => (
          <div
            key={node.fullPath}
            className={`${styles.treeRow} ${selectedPaths.has(node.fullPath) ? styles.selected : ""}`}
            style={{ paddingLeft: "8px" }}
            onClick={(e) => onSelect(node, e)}
            onContextMenu={(e) => { e.preventDefault(); onSelect(node, e); onContextMenu?.(node, e); }}
            title={`ID: ${node.asset?.id}${(node.asset?.spans.length ?? 0) > 1 ? ` · ${node.asset!.spans.length} spans` : ""}`}
          >
            <span className={styles.fileIcon}>·</span>
            <span className={styles.nodeName}>{node.fullPath}</span>
          </div>
        ))}
        {results.length >= 200 && (
          <div className={styles.treeRow} style={{ paddingLeft: "8px", opacity: 0.5 }}>
            <span className={styles.nodeName}>... additional results omitted for performance</span>
          </div>
        )}
      </div>
    );
  }

  const sortedChildren = Array.from(root.children.values()).sort((a, b) => {
    const aIsFolder = a.children.size > 0;
    const bIsFolder = b.children.size > 0;
    if (aIsFolder !== bIsFolder) return aIsFolder ? -1 : 1;
    return a.name.localeCompare(b.name);
  });

  return (
    <>
      {sortedChildren.map((child) => (
        <TreeNode
          key={child.fullPath}
          node={child}
          selectedPaths={selectedPaths}
          onSelect={onSelect}
          onContextMenu={onContextMenu}
          filter=""
          depth={depth}
        />
      ))}
    </>
  );
}
