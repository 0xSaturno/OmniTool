import type { ReactNode } from "react";
import { LuPackagePlus } from "react-icons/lu";
import { RiArchiveStackLine } from "react-icons/ri";
import { MdTexture } from "react-icons/md";
import { TbHexagon3D } from "react-icons/tb";
import { VscJson } from "react-icons/vsc";

export interface ToolDefinition {
  id: string;
  label: string;
  description: string;
  path: string;
  icon: ReactNode;
  category: "model" | "animation" | "archive" | "config" | "misc";
}

export const TOOLS: ToolDefinition[] = [
  {
    id: "model-converter",
    label: "Model Converter",
    description: "Convert .model ↔ .ascii (export and inject mesh assets)",
    path: "/tools/model-converter",
    icon: <TbHexagon3D />,
    category: "model",
  },
  {
    id: "material-remapper",
    label: "Material Remapper",
    description: "Remap material path references inside .model files",
    path: "/tools/material-remapper",
    icon: <MdTexture />,
    category: "model",
  },
  {
    id: "asset-browser",
    label: "Asset Browser",
    description: "Browse and extract game assets",
    path: "/tools/asset-browser",
    icon: <RiArchiveStackLine />,
    category: "archive",
  },
  {
    id: "stager",
    label: "Stager",
    description: "Manage modding projects and export .stage packages",
    path: "/tools/stager",
    icon: <LuPackagePlus />,
    category: "archive",
  },
  {
    id: "config-editor",
    label: "Config Editor",
    description: "Read and edit .config files as JSON",
    path: "/tools/config-editor",
    icon: <VscJson />,
    category: "config",
  },
];
