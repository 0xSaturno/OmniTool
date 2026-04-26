import React, { createContext, useContext, useState, useEffect } from "react";

export interface AppSettings {
  archivesDir: string;
}

interface SettingsContextValue {
  settings: AppSettings;
  updateSettings: (newSettings: Partial<AppSettings>) => void;
  isSettingsOpen: boolean;
  setSettingsOpen: (open: boolean) => void;
}

const defaultSettings: AppSettings = {
  archivesDir: "",
};

const SettingsContext = createContext<SettingsContextValue | null>(null);

function loadSettings(): AppSettings {
  const saved = localStorage.getItem("rcra_settings");
  if (saved) {
    try {
      return { ...defaultSettings, ...JSON.parse(saved) };
    } catch {}
  }
  return defaultSettings;
}

export function SettingsProvider({ children }: { children: React.ReactNode }) {
  const [settings, setSettings] = useState<AppSettings>(loadSettings);
  const [isSettingsOpen, setSettingsOpen] = useState(false);

  useEffect(() => {
    localStorage.setItem("rcra_settings", JSON.stringify(settings));
  }, [settings]);

  const updateSettings = (newSettings: Partial<AppSettings>) => {
    setSettings((prev) => ({ ...prev, ...newSettings }));
  };

  return (
    <SettingsContext.Provider value={{ settings, updateSettings, isSettingsOpen, setSettingsOpen }}>
      {children}
    </SettingsContext.Provider>
  );
}

export function useSettings() {
  const context = useContext(SettingsContext);
  if (!context) throw new Error("useSettings must be used within SettingsProvider");
  return context;
}
