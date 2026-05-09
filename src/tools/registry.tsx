import type { ReactNode } from "react";
import { LuPackagePlus, LuLightbulb } from "react-icons/lu";
import { RiArchiveStackLine, RiCloudyLine } from "react-icons/ri";
import { TbHexagon3D } from "react-icons/tb";
import { MdTexture } from "react-icons/md";
import { PiImageSquareBold } from "react-icons/pi";
import { VscJson } from "react-icons/vsc";

export interface ToolDefinition {
  id: string;
  label: string;
  description: string;
  path: string;
  icon: ReactNode;
  category: "asset" | "model" | "texture" | "config" | "misc";
}

export const TOOLS: ToolDefinition[] = [
  {
    id: "asset-browser",
    label: "Asset Browser",
    description: "Browse and extract game assets",
    path: "/tools/asset-browser",
    icon: <RiArchiveStackLine />,
    category: "asset",
  },
  {
    id: "stager",
    label: "Stager",
    description: "Manage modding projects and export .stage packages",
    path: "/tools/stager",
    icon: <LuPackagePlus />,
    category: "asset",
  },
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
    id: "texture-converter",
    label: "Texture Converter",
    description: "Extract and Replace .texture files to/from .dds",
    path: "/tools/texture-converter",
    icon: <PiImageSquareBold />,
    category: "texture",
  },
  {
    id: "config-editor",
    label: "Config Editor",
    description: "Read and edit .config files as JSON",
    path: "/tools/config-editor",
    icon: <VscJson />,
    category: "config",
  },
  {
    id: "atmosphere-editor",
    label: "Atmosphere Editor",
    description: "Edit .atmosphere known values",
    path: "/tools/atmosphere-editor",
    icon: <RiCloudyLine />,
    category: "misc",
  },
  {
    id: "zonelightbin-module",
    label: "ZoneLightBin",
    description: "under construction",
    path: "/tools/zonelightbin-module",
    icon: <LuLightbulb />,
    category: "misc",
  },
];
