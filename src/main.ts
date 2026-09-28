import { createApp } from "vue";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import MainApp from "./windows/MainApp.vue";
import BatchManageApp from "./windows/BatchManageApp.vue";
import AdvancedBackupApp from "./windows/AdvancedBackupApp.vue";
import "./styles.css";

const label = getCurrentWebviewWindow().label;
const Root =
  label === "batches" ? BatchManageApp :
  label === "adv-backup" ? AdvancedBackupApp :
  MainApp;
createApp(Root).mount("#app");