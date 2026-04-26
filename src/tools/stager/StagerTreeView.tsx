import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useNavigate } from "react-router-dom";
import SendToStagerModal from "../../components/shared/SendToStagerModal";
import styles from "./StagerTreeView.module.css";

interface TreeNode {
  name: string;
  path: string;
  isFolder: boolean;
  children: Map<string, TreeNode>;
}

const SEND_TO_ROUTES: Record<string, { label: string; route: string }> = {
  config:   { label: "Config Editor",      route: "/tools/config-editor" },
  model:    { label: "Model Converter",    route: "/tools/model-converter" },
  material: { label: "Material Remapper",  route: "/tools/material-remapper" },
};

function extOf(name: string) {
  return name.split(".").pop()?.toLowerCase() ?? "";
}

export default function StagerTreeView({
  project,
  assets,
  onRefresh,
}: {
  project: string;
  assets: string[];
  onRefresh: () => void;
}) {
  const navigate = useNavigate();
  const [tree, setTree] = useState<TreeNode>({ name: "", path: "", isFolder: true, children: new Map() });
  const [menu, setMenu] = useState<{ x: number; y: number; node: TreeNode } | null>(null);
  const [renamingPath, setRenamingPath] = useState<string | null>(null);
  const [sendToStager, setSendToStager] = useState<{ file: string; defaultPath: string } | null>(null);

  useEffect(() => {
    const root: TreeNode = { name: "", path: "", isFolder: true, children: new Map() };
    for (const asset of assets) {
      const parts = asset.split("/");
      let current = root;
      let currentPath = "";
      for (let i = 0; i < parts.length; i++) {
        const part = parts[i];
        currentPath = currentPath ? `${currentPath}/${part}` : part;
        const isFolder = i < parts.length - 1;
        let child = current.children.get(part);
        if (!child) {
          child = { name: part, path: currentPath, isFolder, children: new Map() };
          current.children.set(part, child);
        }
        current = child;
      }
    }
    setTree(root);
  }, [assets]);

  useEffect(() => {
    const unlistenDrop = listen("tauri://drag-drop", async (event: any) => {
      if (event.payload?.paths?.length > 0) {
        try {
          await invoke("import_assets_to_project", {
            name: project,
            paths: event.payload.paths,
            targetFolder: "0",
          });
          onRefresh();
        } catch (e) {
          console.error(e);
        }
      }
    });

    const clickAway = () => setMenu(null);
    window.addEventListener("click", clickAway);
    return () => {
      unlistenDrop.then((f) => f());
      window.removeEventListener("click", clickAway);
    };
  }, [project, onRefresh]);

  async function handleRenameCommit(node: TreeNode, newName: string) {
    setRenamingPath(null);
    if (!newName || newName === node.name) return;
    try {
      const idx = node.path.lastIndexOf(node.name);
      const newPath = idx !== -1 ? node.path.substring(0, idx) + newName : newName;
      await invoke("rename_project_asset", { name: project, oldPath: node.path, newPath });
      onRefresh();
    } catch (e) {
      console.error("Rename failed:", e);
    }
  }

  async function handleDelete(node: TreeNode) {
    setMenu(null);
    try {
      await invoke("delete_project_asset", { name: project, path: node.path });
      onRefresh();
    } catch (e) {
      console.error("Delete failed:", e);
    }
  }

  async function handleSendToTool(node: TreeNode, route: string) {
    setMenu(null);
    try {
      const projectDir: string = await invoke("get_project_path", { name: project });
      const absolutePath = `${projectDir}\\${node.path.replace(/\//g, "\\")}`;
      navigate(route, { state: { filePath: absolutePath } });
    } catch (e) {
      console.error("Send to tool failed:", e);
    }
  }

  function handleContextMenu(node: TreeNode, e: React.MouseEvent) {
    e.preventDefault();
    setMenu({ x: e.clientX, y: e.clientY, node });
  }

  function renderTree(node: TreeNode, depth: number) {
    return Array.from(node.children.values())
      .sort((a, b) => {
        if (a.isFolder !== b.isFolder) return a.isFolder ? -1 : 1;
        return a.name.localeCompare(b.name);
      })
      .map((child) => (
        <TreeItem
          key={child.path}
          node={child}
          depth={depth}
          renamingPath={renamingPath}
          onContextMenu={handleContextMenu}
          onStartRename={(path) => setRenamingPath(path)}
          onRenameCommit={handleRenameCommit}
          onRenameCancel={() => setRenamingPath(null)}
        />
      ));
  }

  const menuNode = menu?.node;
  const sendToTargets = menuNode && !menuNode.isFolder
    ? Object.entries(SEND_TO_ROUTES).filter(([ext]) => extOf(menuNode.name) === ext)
    : [];

  return (
    <div style={{ position: "relative", height: "100%", width: "100%", overflowY: "auto", overflowX: "hidden" }}>
      {assets.length === 0 ? (
        <div style={{ padding: "1rem", color: "var(--text-muted)", fontSize: "0.85rem", textAlign: "center", marginTop: "2rem" }}>
          Empty project.<br /><br />Drag & Drop files and folders directly inside this window to import them instantly!
        </div>
      ) : (
        renderTree(tree, 0)
      )}

      {menu && (
        <div className={styles.contextMenu} style={{ top: menu.y, left: menu.x }}>
          <div className={styles.menuItem} onClick={() => { setMenu(null); setRenamingPath(menu.node.path); }}>
            Rename
          </div>

          {sendToTargets.length > 0 && (
            <>
              <div className={styles.menuDivider} />
              <div className={styles.menuSection}>Send To</div>
              {sendToTargets.map(([, target]) => (
                <div
                  key={target.route}
                  className={styles.menuItem}
                  onClick={() => handleSendToTool(menu.node, target.route)}
                >
                  {target.label}
                </div>
              ))}
              <div
                className={styles.menuItem}
                onClick={async () => {
                  setMenu(null);
                  const projectDir: string = await invoke("get_project_path", { name: project });
                  const abs = `${projectDir}\\${menu.node.path.replace(/\//g, "\\")}`;
                  setSendToStager({ file: abs, defaultPath: menu.node.path });
                }}
              >
                Stager (copy)
              </div>
            </>
          )}

          <div className={styles.menuDivider} />
          <div className={`${styles.menuItem} ${styles.danger}`} onClick={() => handleDelete(menu.node)}>
            Delete
          </div>
        </div>
      )}

      {sendToStager && (
        <SendToStagerModal
          sourceFile={sendToStager.file}
          defaultTargetPath={sendToStager.defaultPath}
          onClose={() => setSendToStager(null)}
          onSent={() => onRefresh()}
        />
      )}
    </div>
  );
}

function TreeItem({
  node,
  depth,
  renamingPath,
  onContextMenu,
  onStartRename,
  onRenameCommit,
  onRenameCancel,
}: {
  node: TreeNode;
  depth: number;
  renamingPath: string | null;
  onContextMenu: (node: TreeNode, e: React.MouseEvent) => void;
  onStartRename: (path: string) => void;
  onRenameCommit: (node: TreeNode, newName: string) => void;
  onRenameCancel: () => void;
}) {
  const [open, setOpen] = useState(depth < 2);
  const inputRef = useRef<HTMLInputElement>(null);
  const isRenaming = renamingPath === node.path;

  useEffect(() => {
    if (isRenaming) inputRef.current?.select();
  }, [isRenaming]);

  return (
    <div className={styles.treeNode}>
      <div
        className={styles.treeRow}
        style={{ paddingLeft: `${depth * 14 + 4}px` }}
        onClick={() => node.isFolder && setOpen(!open)}
        onDoubleClick={(e) => { e.stopPropagation(); if (!node.isFolder) onStartRename(node.path); }}
        onContextMenu={(e) => onContextMenu(node, e)}
      >
        <span className={styles.folderToggle}>{node.isFolder ? (open ? "▾" : "▸") : ""}</span>
        <span className={node.isFolder ? styles.folderIcon : styles.fileIcon}>{node.isFolder ? "📁" : "📄"}</span>
        {isRenaming ? (
          <input
            ref={inputRef}
            className={styles.renameInput}
            defaultValue={node.name}
            onClick={(e) => e.stopPropagation()}
            onKeyDown={(e) => {
              if (e.key === "Enter") onRenameCommit(node, e.currentTarget.value);
              else if (e.key === "Escape") onRenameCancel();
            }}
            onBlur={(e) => onRenameCommit(node, e.currentTarget.value)}
          />
        ) : (
          <span className={styles.nodeName}>{node.name}</span>
        )}
      </div>
      {node.isFolder && open && (
        <div className={styles.children}>
          {Array.from(node.children.values())
            .sort((a, b) => {
              if (a.isFolder !== b.isFolder) return a.isFolder ? -1 : 1;
              return a.name.localeCompare(b.name);
            })
            .map((child) => (
              <TreeItem
                key={child.path}
                node={child}
                depth={depth + 1}
                renamingPath={renamingPath}
                onContextMenu={onContextMenu}
                onStartRename={onStartRename}
                onRenameCommit={onRenameCommit}
                onRenameCancel={onRenameCancel}
              />
            ))}
        </div>
      )}
    </div>
  );
}
