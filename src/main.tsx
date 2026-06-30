import React from "react";
import ReactDOM from "react-dom/client";
import { HashRouter } from "react-router-dom";
import App from "./App";
import { SettingsProvider } from "./contexts/SettingsContext";
import { ProjectsProvider } from "./contexts/ProjectsContext";
import { disableBrowserDefaults } from "./utils/disableBrowserDefaults";
import "./styles/global.css";

disableBrowserDefaults();

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <SettingsProvider>
      <ProjectsProvider>
        <HashRouter>
          <App />
        </HashRouter>
      </ProjectsProvider>
    </SettingsProvider>
  </React.StrictMode>
);
