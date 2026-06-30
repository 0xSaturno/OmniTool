import { createContext, useContext, useState, useEffect, ReactNode, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

export interface ProjectInfo {
  name: string;
  game: string;
  author: string;
  version: string;
}

interface ProjectsContextType {
  projects: ProjectInfo[];
  selectedProject: string;
  setSelectedProject: (name: string) => void;
  refreshProjects: () => Promise<void>;
  createProject: (name: string, author: string) => Promise<string>;
  deleteProject: (name: string) => Promise<void>;
}

const ProjectsContext = createContext<ProjectsContextType | undefined>(undefined);

export function ProjectsProvider({ children }: { children: ReactNode }) {
  const [projects, setProjects] = useState<ProjectInfo[]>([]);
  const [selectedProject, setSelectedProject] = useState<string>("");

  const refreshProjects = useCallback(async () => {
    try {
      const list = await invoke<ProjectInfo[]>("list_projects");
      setProjects(list);

      // Update selected project to first available if empty or deleted
      setSelectedProject(prev => {
        if (!prev) {
          return list.length > 0 ? list[0].name : "";
        }
        if (!list.some(p => p.name === prev)) {
          return list.length > 0 ? list[0].name : "";
        }
        return prev;
      });
    } catch (e) {
      console.error("Failed to load projects", e);
    }
  }, []);

  const createProject = useCallback(async (name: string, author: string) => {
    const projectPath = await invoke<string>("create_project", {
      name,
      game: "RCRA",
      author,
    });
    await refreshProjects();
    setSelectedProject(name);
    return projectPath;
  }, [refreshProjects]);

  const deleteProject = useCallback(async (name: string) => {
    await invoke("delete_project", { name });
    await refreshProjects();
  }, [refreshProjects]);

  // Poll for external changes every 3 seconds
  useEffect(() => {
    refreshProjects();
    const interval = setInterval(refreshProjects, 3000);
    return () => clearInterval(interval);
  }, [refreshProjects]);

  return (
    <ProjectsContext.Provider value={{
      projects,
      selectedProject,
      setSelectedProject,
      refreshProjects,
      createProject,
      deleteProject,
    }}>
      {children}
    </ProjectsContext.Provider>
  );
}

export function useProjects() {
  const context = useContext(ProjectsContext);
  if (!context) {
    throw new Error("useProjects must be used within a ProjectsProvider");
  }
  return context;
}
