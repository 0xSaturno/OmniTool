import { useState } from "react";
import { useProjects } from "../../contexts/ProjectsContext";
import { invoke } from "@tauri-apps/api/core";
import styles from "./SendToStagerModal.module.css";

interface Props {
  sourceFile: string;
  defaultTargetPath: string;
  onClose: () => void;
  onSent: (projectName: string) => void;
}

export default function SendToStagerModal({ sourceFile, defaultTargetPath, onClose, onSent }: Props) {
  const { projects, selectedProject, setSelectedProject, createProject } = useProjects();
  const [targetPath, setTargetPath] = useState(defaultTargetPath);
  const [sending, setSending] = useState(false);
  const [error, setError] = useState("");

  const [showNewProject, setShowNewProject] = useState(false);
  const [newProjName, setNewProjName] = useState("");
  const [newProjAuthor, setNewProjAuthor] = useState("");
  const [creatingProject, setCreatingProject] = useState(false);

  async function handleCreateProject() {
    if (!newProjName.trim()) return;
    setCreatingProject(true);
    setError("");
    try {
      await createProject(newProjName.trim(), newProjAuthor.trim());
      setShowNewProject(false);
      setNewProjName("");
      setNewProjAuthor("");
    } catch (e) {
      setError(`Failed to create project: ${e}`);
    } finally {
      setCreatingProject(false);
    }
  }

  async function handleSend() {
    if (!selectedProject || !targetPath.trim()) return;
    setSending(true);
    setError("");
    try {
      await invoke("import_file_to_project", {
        name: selectedProject,
        sourcePath: sourceFile,
        targetPath: targetPath.trim(),
      });
      onSent(selectedProject);
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setSending(false);
    }
  }

  function handleBackdropClick(e: React.MouseEvent) {
    if (e.target === e.currentTarget) onClose();
  }

  return (
    <div className={styles.backdrop} onClick={handleBackdropClick}>
      <div className={styles.modal}>
        <h3 className={styles.title}>Send to Stager</h3>

        <div className={styles.field}>
          <label className={styles.label}>Project</label>
          {showNewProject ? (
            <div className={styles.newProjectForm}>
              <input
                className={styles.newProjInputFull}
                placeholder="Project name"
                value={newProjName}
                onChange={(e) => setNewProjName(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && handleCreateProject()}
                autoFocus
              />
              <input
                className={styles.newProjInputFull}
                placeholder="Author"
                value={newProjAuthor}
                onChange={(e) => setNewProjAuthor(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && handleCreateProject()}
              />
              <div className={styles.formActions}>
                <button
                  className={styles.sendBtn}
                  style={{ padding: "0.4rem", flex: 1 }}
                  onClick={handleCreateProject}
                  disabled={creatingProject || !newProjName.trim()}
                >
                  {creatingProject ? "Creating…" : "Create"}
                </button>
                <button
                  className={styles.cancelProjBtn}
                  onClick={() => { setShowNewProject(false); setNewProjName(""); setNewProjAuthor(""); }}
                >
                  Cancel
                </button>
              </div>
            </div>
          ) : (
            <div className={styles.projectPickerRow}>
              {projects.length === 0 ? (
                <span className={styles.empty} style={{ flex: 1 }}>No projects found.</span>
              ) : (
                <select
                  className={styles.select}
                  style={{ flex: 1 }}
                  value={selectedProject}
                  onChange={(e) => setSelectedProject(e.target.value)}
                >
                  {projects.map((p) => (
                    <option key={p.name} value={p.name}>{p.name}</option>
                  ))}
                </select>
              )}
              <button
                className={styles.newProjBtnSmall}
                onClick={() => setShowNewProject(true)}
                title="Create a new project"
                type="button"
              >
                + New
              </button>
            </div>
          )}
        </div>

        <div className={styles.field}>
          <label className={styles.label}>Path in project</label>
          <input
            className={styles.input}
            value={targetPath}
            onChange={(e) => setTargetPath(e.target.value)}
            spellCheck={false}
            placeholder="e.g. 0/characters/hero.model"
          />
          <span className={styles.hint}>Include span prefix (0/ = SD, 1/ = HD)</span>
        </div>

        {error && <span className={styles.error}>{error}</span>}

        <div className={styles.actions}>
          <button className={styles.cancelBtn} onClick={onClose} disabled={sending}>
            Cancel
          </button>
          <button
            className={styles.sendBtn}
            onClick={handleSend}
            disabled={sending || !selectedProject || !targetPath.trim()}
          >
            {sending ? "Sending…" : "Send"}
          </button>
        </div>
      </div>
    </div>
  );
}
