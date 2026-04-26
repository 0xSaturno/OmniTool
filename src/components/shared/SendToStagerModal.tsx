import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import styles from "./SendToStagerModal.module.css";

interface Project {
  name: string;
  game: string;
  author: string;
}

interface Props {
  sourceFile: string;
  defaultTargetPath: string;
  onClose: () => void;
  onSent: (projectName: string) => void;
}

export default function SendToStagerModal({ sourceFile, defaultTargetPath, onClose, onSent }: Props) {
  const [projects, setProjects] = useState<Project[]>([]);
  const [selectedProject, setSelectedProject] = useState("");
  const [targetPath, setTargetPath] = useState(defaultTargetPath);
  const [sending, setSending] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    invoke<Project[]>("list_projects").then((list) => {
      setProjects(list);
      if (list.length > 0) setSelectedProject(list[0].name);
    });
  }, []);

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
          {projects.length === 0 ? (
            <span className={styles.empty}>No projects — create one in Stager first.</span>
          ) : (
            <select
              className={styles.select}
              value={selectedProject}
              onChange={(e) => setSelectedProject(e.target.value)}
            >
              {projects.map((p) => (
                <option key={p.name} value={p.name}>{p.name}</option>
              ))}
            </select>
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
