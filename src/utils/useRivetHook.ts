import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/** Whether rivet_hook.dll sits in the game folder; null until checked. Rechecks when the window regains focus. */
export function useRivetHookInstalled(gameDir: string): boolean | null {
  const [installed, setInstalled] = useState<boolean | null>(null);

  useEffect(() => {
    if (!gameDir) {
      setInstalled(false);
      return;
    }
    let live = true;
    const check = () =>
      invoke<boolean>("rivet_hook_installed", { gameDir })
        .then((v) => live && setInstalled(v))
        .catch(() => live && setInstalled(false));
    check();
    window.addEventListener("focus", check);
    return () => {
      live = false;
      window.removeEventListener("focus", check);
    };
  }, [gameDir]);

  return installed;
}
