import { WebviewWindow } from "@tauri-apps/api/webviewWindow";

export interface ToolLaunchPayload {
  filePath?: string;
  assetPath?: string;
}

function toWindowLabel(path: string): string {
  const slug = path
    .replace(/^\/+/, "")
    .replace(/[^a-zA-Z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .toLowerCase();
  return `tool-${slug}-${Date.now()}`;
}

function buildToolUrl(path: string, payload?: ToolLaunchPayload): string {
  const search = new URLSearchParams();
  if (payload?.filePath) search.set("filePath", payload.filePath);
  if (payload?.assetPath) search.set("assetPath", payload.assetPath);

  const query = search.toString();
  return `/#${path}${query ? `?${query}` : ""}`;
}

export function openToolWindow(path: string, payload?: ToolLaunchPayload, launchInNewWindow = true) {
  const toolUrl = buildToolUrl(path, payload);

  if (!launchInNewWindow) {
    window.location.assign(toolUrl);
    return null;
  }

  const toolWindow = new WebviewWindow(toWindowLabel(path), {
    url: toolUrl,
    title: "OmniTool",
    width: 1400,
    height: 800,
    minWidth: 900,
    minHeight: 600,
    resizable: true,
    decorations: false,
    center: true,
    backgroundColor: [15, 17, 23],
  });

  toolWindow.once("tauri://error", (event) => {
    console.error("[openToolWindow] failed to create tool window", {
      path,
      payload,
      event,
    });
  });

  return toolWindow;
}
