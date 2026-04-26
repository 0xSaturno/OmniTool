import type { ReactNode } from "react";
import Sidebar from "./Sidebar";
import styles from "./Shell.module.css";

export default function Shell({ children }: { children: ReactNode }) {
  return (
    <div className={styles.shell}>
      <Sidebar />
      <main className={styles.main}>{children}</main>
    </div>
  );
}
