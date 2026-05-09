import { Routes, Route, Navigate, useLocation } from "react-router-dom";
import Shell from "./components/layout/Shell";
import Home from "./pages/Home";
import ModelConverter from "./tools/model-converter/ModelConverter";
import AssetBrowser from "./tools/asset-browser/AssetBrowser";
import MaterialRemapper from "./tools/material-remapper/MaterialRemapper";
import Stager from "./tools/stager/Stager";
import ConfigEditor from "./tools/config-editor/ConfigEditor";
import AtmosphereEditor from "./tools/atmosphere-editor/AtmosphereEditor";
import ZoneLightBinModule from "./tools/zonelightbin-module/ZoneLightBinModule";
import TextureConverter from "./tools/texture-converter/TextureConverter";
import SettingsModal from "./components/shared/SettingsModal";

import Titlebar from "./components/layout/Titlebar";

const TOOL_PATHS = [
  "/tools/model-converter",
  "/tools/material-remapper",
  "/tools/asset-browser",
  "/tools/texture-converter",
  "/tools/stager",
  "/tools/config-editor",
  "/tools/atmosphere-editor",
  "/tools/zonelightbin-module",
] as const;

type ToolPath = (typeof TOOL_PATHS)[number];

function show(active: ToolPath, current: string) {
  return current === active ? "contents" : "none";
}

export default function App() {
  const { pathname } = useLocation();

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100vh" }}>
      <Titlebar />
      <div style={{ flex: 1, overflow: "hidden" }}>
        <Shell>
          <SettingsModal />

          <div style={{ display: show("/tools/model-converter", pathname) }}>
            <ModelConverter />
          </div>
          <div style={{ display: show("/tools/material-remapper", pathname) }}>
            <MaterialRemapper />
          </div>
          <div style={{ display: show("/tools/asset-browser", pathname) }}>
            <AssetBrowser />
          </div>
          <div style={{ display: show("/tools/texture-converter", pathname) }}>
            <TextureConverter />
          </div>
          <div style={{ display: show("/tools/stager", pathname) }}>
            <Stager />
          </div>
          <div style={{ display: show("/tools/config-editor", pathname) }}>
            <ConfigEditor />
          </div>
          <div style={{ display: show("/tools/atmosphere-editor", pathname) }}>
            <AtmosphereEditor />
          </div>
          <div style={{ display: show("/tools/zonelightbin-module", pathname) }}>
            <ZoneLightBinModule />
          </div>

          <Routes>
            <Route path="/" element={<Home />} />
            {TOOL_PATHS.map((p) => (
              <Route key={p} path={p} element={null} />
            ))}
            <Route path="*" element={<Navigate to="/" replace />} />
          </Routes>
        </Shell>
      </div>
    </div>
  );
}
