export interface CloseRequestedEvent {
  preventDefault(): void;
}

export interface FocusChangedEvent {
  payload: boolean;
}

export type UnlistenFn = () => void;

const noop = async (): Promise<void> => {};
const noopListen = async (): Promise<UnlistenFn> => () => {};

const webWindow = {
  show: noop,
  hide: noop,
  close: noop,
  minimize: noop,
  toggleMaximize: noop,
  onCloseRequested: (_handler: (event: CloseRequestedEvent) => void) => noopListen(),
  onFocusChanged: (_handler: (event: FocusChangedEvent) => void) => noopListen(),
};

export function getCurrentWindow() {
  return webWindow;
}
