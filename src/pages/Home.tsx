import { useNavigate } from "react-router-dom";
import { TOOLS } from "../tools/registry";
import styles from "./Home.module.css";
import { BsWrenchAdjustableCircle } from "react-icons/bs";

export default function Home() {
  const navigate = useNavigate();
  return (
    <div className={styles.home}>
      <h1 className={styles.title}><BsWrenchAdjustableCircle /> OmniTool</h1>
      <p className={styles.subtitle}>
        Ratchet &amp; Clank: Rift Apart modding suite
      </p>

      <div className={styles.grid}>
        {TOOLS.map((tool) => (
          <button key={tool.id} className={styles.card} onClick={() => navigate(tool.path)}>
            <span className={styles.cardIcon}>{tool.icon}</span>
            <span className={styles.cardLabel}>{tool.label}</span>
            <span className={styles.cardDesc}>{tool.description}</span>
          </button>
        ))}
      </div>
    </div>
  );
}
