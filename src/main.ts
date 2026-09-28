import { createApp } from "vue";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import MainApp from "./windows/MainApp.vue";
import BackupApp from "./windows/BackupApp.vue";
import VerifyApp from "./windows/VerifyApp.vue";
import "./styles.css";

const label = getCurrentWebviewWindow().label;
const Root = label === "backup" ? BackupApp : label === "verify" ? VerifyApp : MainApp;
createApp(Root).mount("#app");
