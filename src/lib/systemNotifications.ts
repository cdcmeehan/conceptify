import { isTauri } from "@tauri-apps/api/core";
import {
  Visibility,
  isPermissionGranted,
  onAction,
  registerActionTypes,
  requestPermission,
  sendNotification,
  type Options,
} from "@tauri-apps/plugin-notification";
import * as api from "./api";
import { appStore } from "../store/appStore";

const ACTION_TYPE = "conceptify-run";
let enabled = false;

function hasNativeNotificationBridge(): boolean {
  // The browser QA harness shims core Tauri invoke over 127.0.0.1 but cannot
  // expose native plugins. Production desktop builds use the Tauri scheme.
  return isTauri() && !(location.protocol === "http:" && location.hostname === "127.0.0.1");
}

export function setSystemNotificationsEnabled(value: boolean): void {
  enabled = value;
}

/** Called only from the user's explicit enable action in Settings. */
export async function requestSystemNotificationPermission(): Promise<boolean> {
  if (!hasNativeNotificationBridge()) return false;
  if (await isPermissionGranted()) return true;
  return (await requestPermission()) === "granted";
}

export async function initSystemNotifications(): Promise<() => void> {
  try {
    enabled = (await api.getAgentSettings()).systemNotifications;
  } catch {
    enabled = false;
  }
  if (!hasNativeNotificationBridge()) return () => {};

  // The explicit Open action is foregrounded on platforms that support action
  // buttons. hiddenPreviewsBodyPlaceholder keeps lock-screen previews generic.
  await registerActionTypes([
    {
      id: ACTION_TYPE,
      actions: [{ id: "open", title: "Open", foreground: true }],
      hiddenPreviewsBodyPlaceholder: "Open Conceptify to view this activity.",
      hiddenPreviewsShowTitle: false,
      hiddenPreviewsShowSubtitle: false,
    },
  ]);
  const listener = await onAction((notification: Options) => {
    const projectId = notification.extra?.projectId;
    const threadId = notification.extra?.threadId;
    if (typeof projectId === "string" && typeof threadId === "string") {
      void appStore.jumpToProjectThread(projectId, threadId);
    }
  });
  return () => listener.unregister();
}

export async function notifyTerminalRun(runId: string): Promise<void> {
  if (!enabled || !hasNativeNotificationBridge()) return;
  try {
    if (!(await isPermissionGranted())) return;
    const item = await api.claimSystemRunNotification(runId);
    if (item == null) return; // already delivered, cancelled, or non-terminal

    const permissionNeeded =
      item.status === "failed" && item.status_reason?.toLowerCase().includes("permission");
    const title = permissionNeeded
      ? "Conceptify needs attention"
      : item.status === "completed"
        ? "Conceptify finished a run"
        : item.status === "conflicted"
          ? "Conceptify found a conflict"
          : "Conceptify run needs attention";

    sendNotification({
      title,
      // Never include the thread title, question, error reason, model, or path:
      // those can reveal prompt content on a lock screen. The opaque payload is
      // used only after the user opens the notification.
      body: `${item.project_name} · Open Conceptify to view the thread.`,
      visibility: Visibility.Private,
      actionTypeId: ACTION_TYPE,
      autoCancel: true,
      group: item.project_id,
      extra: {
        runId: item.run_id,
        projectId: item.project_id,
        threadId: item.thread_id,
      },
    });
  } catch (error) {
    // Native notifications are optional. The durable in-app attention item is
    // still authoritative and must not be obscured by an OS integration error.
    console.warn("system notification unavailable", error);
  }
}
