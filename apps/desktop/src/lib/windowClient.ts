import { invoke } from "@tauri-apps/api/core";

export interface NativeWindowClient {
  openNewWindow: () => Promise<string>;
}

type InvokeCommand = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;

export function createNativeWindowClient(
  invokeCommand: InvokeCommand = invoke,
): NativeWindowClient {
  return {
    openNewWindow() {
      return invokeCommand<string>("open_new_window");
    },
  };
}

export const tauriNativeWindowClient = createNativeWindowClient();
