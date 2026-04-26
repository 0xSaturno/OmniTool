import { Routes, Route, Navigate, useLocation } from "react-router-dom";
import Shell from "./components/layout/Shell";
import Home from "./pages/Home";
import ModelConverter from "./tools/model-converter/ModelConverter";
import AssetBrowser from "./tools/asset-browser/AssetBrowser";
import MaterialRemapper from "./tools/material-remapper/MaterialRemapper";
import Stager from "./tools/stager/Stager";
import ConfigEditor from "./tools/config-editor/ConfigEditor";
import SettingsModal from "./components/shared/SettingsModal";

import Titlebar from "./components/layout/Titlebar";

export default function App() {
  const location = useLocation();
  const isAssetBrowser = location.pathname === "/tools/asset-browser";

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100vh" }}>
      <Titlebar />
      <div style={{ flex: 1, overflow: "hidden" }}>
        <Shell>
          <SettingsModal />
          <div style={{ display: isAssetBrowser ? "contents" : "none" }}>
            <AssetBrowser />
          </div>
          <Routes>
            <Route path="/" element={<Home />} />
            <Route path="/tools/model-converter" element={<ModelConverter />} />
            <Route path="/tools/material-remapper" element={<MaterialRemapper />} />
            <Route path="/tools/asset-browser" element={null} />
            <Route path="/tools/stager" element={<Stager />} />
            <Route path="/tools/config-editor" element={<ConfigEditor />} />
            <Route path="*" element={<Navigate to="/" replace />} />
          </Routes>
        </Shell>
      </div>
    </div>
  );
}

