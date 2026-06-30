export const SEND_TO_ROUTES: Record<string, { label: string; route: string }[]> = {
  config: [{ label: "Config Editor", route: "/tools/config-editor" }],
  actor: [{ label: "Config Editor", route: "/tools/config-editor" }],
  conduit: [{ label: "Config Editor", route: "/tools/config-editor" }],
  performanceset: [{ label: "Config Editor", route: "/tools/config-editor" }],
  model: [
    { label: "Model Converter", route: "/tools/model-converter" },
    { label: "Material Remapper", route: "/tools/material-remapper" },
  ],
  atmosphere: [{ label: "Atmosphere Editor", route: "/tools/atmosphere-editor" }],
  zonelightbin: [{ label: "ZoneLightBin Module", route: "/tools/zonelightbin-module" }],
  soundbank: [
    { label: "Wwise Soundbank Patcher", route: "/tools/wwise-patcher" },
    { label: "BNK Explorer", route: "/tools/bnk-explorer" },
  ],
  bnk: [{ label: "BNK Explorer", route: "/tools/bnk-explorer" }],
};
