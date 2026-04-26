import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import StatusLog, { type LogEntry } from "../../components/shared/StatusLog";
import StagerTreeView from "./StagerTreeView";
import styles from "./Stager.module.css";

interface Project {
  name: string;
  game: string;
  author: string;
  version: string;
}

export default function Stager() {
  const [projects, setProjects] = useState<Project[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [assets, setAssets] = useState<string[]>([]);
  const [log, setLog] = useState<LogEntry[]>([]);
  const [exporting, setExporting] = useState(false);

  // New-project form
  const [showForm, setShowForm] = useState(false);
  const [formName, setFormName] = useState("");
  const [formAuthor, setFormAuthor] = useState("");
  const [creating, setCreating] = useState(false);

  // Pending deletion
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);

  function pushLog(type: LogEntry["type"], message: string) {
    setLog((prev) => [...prev, { type, message, ts: Date.now() }]);
  }

  async function loadProjects() {
    try {
      const list: Project[] = await invoke("list_projects");
      setProjects(list);
    } catch (e) {
      pushLog("error", `Failed to list projects: ${e}`);
    }
  }

  async function loadAssets(name: string) {
    try {
      const list: string[] = await invoke("list_project_assets", { name });
      setAssets(list);
    } catch (e) {
      pushLog("error", `Failed to list assets: ${e}`);
      setAssets([]);
    }
  }

  const [editingVersion, setEditingVersion] = useState<string>("");

  useEffect(() => {
    loadProjects();
  }, []);

  useEffect(() => {
    if (selected) loadAssets(selected);
    else setAssets([]);
  }, [selected]);

  async function handleCreate() {
    if (!formName.trim()) return;
    setCreating(true);
    try {
      await invoke("create_project", {
        name: formName.trim(),
        game: "RCRA",
        author: formAuthor.trim(),
      });
      pushLog("success", `Project "${formName.trim()}" created.`);
      setShowForm(false);
      setFormName("");
      setFormAuthor("");
      await loadProjects();
      setSelected(formName.trim());
    } catch (e) {
      pushLog("error", `Failed to create project: ${e}`);
    } finally {
      setCreating(false);
    }
  }

  async function handleDelete(name: string) {
    try {
      await invoke("delete_project", { name });
      pushLog("info", `Project "${name}" deleted.`);
      if (selected === name) {
        setSelected(null);
        setAssets([]);
      }
      setConfirmDelete(null);
      await loadProjects();
    } catch (e) {
      pushLog("error", `Failed to delete project: ${e}`);
    }
  }

  async function handleExport() {
    if (!selected) return;
    try {
      setExporting(true);
      const selectedProject = projects.find(p => p.name === selected);
      const versionSuffix = selectedProject && selectedProject.version !== "" ? `-${selectedProject.version}` : "";
      
      const outputPath = await save({
        filters: [{ name: "Stage File", extensions: ["stage"] }],
        defaultPath: `${selected}${versionSuffix}.stage`,
      });
      if (!outputPath) return;

      setLog([]);
      pushLog("info", `Exporting "${selected}" …`);
      const result: string = await invoke("export_stage", { name: selected, outputPath });
      pushLog("success", `Done → ${result}`);
    } catch (e) {
      pushLog("error", String(e));
    } finally {
      setExporting(false);
    }
  }

  async function handleOpenExplorer() {
    if (!selected) return;
    try {
      await invoke("open_project_in_explorer", { name: selected });
      pushLog("info", `Opened project folder in Explorer.`);
    } catch (e) {
      pushLog("error", `Failed to open explorer: ${e}`);
    }
  }

  // Sync local version draft when project changes
  useEffect(() => {
    if (selected) {
      const p = projects.find(p => p.name === selected);
      if (p) setEditingVersion(p.version || "1.0.0");
    }
  }, [selected, projects]);

  async function handleSaveVersion() {
    if (!selected) return;
    try {
      await invoke("update_project_version", { name: selected, version: editingVersion });
      await loadProjects();
    } catch(e) {
      pushLog("error", `Failed saving version: ${e}`);
    }
  }

  const selectedProject = projects.find((p) => p.name === selected);

  return (
    <div className={styles.page}>
      <div className={styles.header}>
        <h2 className={styles.title}>Stager</h2>
        <span className={styles.subtitle}>Create and manage mod stage packages.</span>
      </div>

      <div className={styles.layout}>
        {/* Left panel */}
        <div className={styles.projectList}>
          <button className={styles.newProjectBtn} onClick={() => setShowForm(true)}>
            + New Project
          </button>

          {showForm && (
            <div className={styles.newProjectForm}>
              <input
                className={styles.formInput}
                placeholder="Project name"
                value={formName}
                onChange={(e) => setFormName(e.target.value)}
                autoFocus
              />
              <input
                className={styles.formInput}
                placeholder="Author"
                value={formAuthor}
                onChange={(e) => setFormAuthor(e.target.value)}
              />
              <div className={styles.formActions}>
                <button className={styles.createBtn} onClick={handleCreate} disabled={creating || !formName.trim()}>
                  {creating ? "Creating…" : "Create"}
                </button>
                <button className={styles.cancelBtn} onClick={() => setShowForm(false)}>
                  Cancel
                </button>
              </div>
            </div>
          )}

          {projects.length === 0 && !showForm && (
            <div className={styles.emptyList}>No projects yet. Create one to get started.</div>
          )}

          {projects.map((p) => (
            <div
              key={p.name}
              className={`${styles.projectCard} ${selected === p.name ? styles.selected : ""}`}
              onClick={() => setSelected(p.name)}
            >
              <div className={styles.projectInfo}>
                <div className={styles.projectName}>{p.name}</div>
                <div className={styles.projectMeta}>{p.game} · {p.author}</div>
              </div>
              {confirmDelete === p.name ? (
                <button
                  className={styles.deleteBtn}
                  style={{ color: "var(--error)" }}
                  onClick={(e) => { e.stopPropagation(); handleDelete(p.name); }}
                >
                  Confirm?
                </button>
              ) : (
                <button
                  className={styles.deleteBtn}
                  onClick={(e) => { e.stopPropagation(); setConfirmDelete(p.name); }}
                  title="Delete project"
                >
                  ✕
                </button>
              )}
            </div>
          ))}
        </div>

        {/* Right panel */}
        {selectedProject ? (
          <div className={styles.workspace}>
            <div className={styles.workspaceHeader}>
              <h2 className={styles.workspaceTitle}>{selectedProject.name}</h2>
              <div style={{ display: "flex", gap: "1rem", alignItems: "center" }}>
                <span className={styles.workspaceMeta}>{selectedProject.game} · {selectedProject.author}</span>
                <div style={{ display: "flex", alignItems: "center", gap: "0.4rem", fontSize: "0.85rem", color: "var(--text-muted)" }}>
                   <span>v</span>
                   <input 
                      className={styles.versionInput}
                      type="text" 
                      value={editingVersion} 
                      onChange={e => setEditingVersion(e.target.value)}
                      onBlur={handleSaveVersion}
                      onKeyDown={e => e.key === "Enter" && e.currentTarget.blur()}
                      style={{ 
                        background: "var(--bg-elevated)", 
                        border: "1px solid var(--border)", 
                        color: "var(--text-primary)", 
                        padding: "2px 6px", 
                        borderRadius: "4px", 
                        width: "120px", 
                        fontSize: "0.85rem",
                        textAlign: "left"
                      }}
                      title="Project Version"
                    />
                </div>
              </div>
            </div>

            <div className={styles.assetSection} style={{ flex: 1, display: 'flex', flexDirection: 'column' }}>
              <div className={styles.assetHeader} style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                <span>Assets ({assets.length} file{assets.length !== 1 ? "s" : ""})</span>
                <button 
                  onClick={handleOpenExplorer} 
                  style={{ background: "transparent", border: "1px solid var(--border)", borderRadius: "4px", padding: "2px 8px", color: "var(--text-secondary)", cursor: "pointer", fontSize: "0.8rem", transition: "0.15s" }}
                  onMouseOver={(e) => (e.currentTarget.style.color = "var(--text-primary)")}
                  onMouseOut={(e) => (e.currentTarget.style.color = "var(--text-secondary)")}
                >
                  Show in Explorer
                </button>
              </div>
              <div className={styles.assetList} style={{ flex: 1, overflow: 'hidden', padding: 0 }}>
                <StagerTreeView 
                   project={selectedProject.name} 
                   assets={assets} 
                   onRefresh={() => loadAssets(selectedProject.name)} 
                />
              </div>
            </div>

            <button className={styles.exportBtn} onClick={handleExport} disabled={exporting || assets.length === 0}>
              {exporting ? "Exporting…" : "Export as .stage"}
            </button>
          </div>
        ) : (
          <div className={styles.placeholder}>Select or create a project to begin.</div>
        )}
      </div>

      <StatusLog entries={log} />
    </div>
  );
}
