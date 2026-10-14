import { createContext, useContext } from "react";

export interface ConfirmOptions {
  title?: string;
  confirmLabel?: string;
  cancelLabel?: string;
  destructive?: boolean;
}

export type ConfirmFn = (message: string, options?: ConfirmOptions) => Promise<boolean>;

/** Supplied by `ConfirmProvider` in ConfirmDialog.tsx, which renders the dialog. */
export const ConfirmContext = createContext<ConfirmFn | null>(null);

export function useConfirm(): ConfirmFn {
  const fn = useContext(ConfirmContext);
  if (!fn) throw new Error("useConfirm must be used within ConfirmProvider");
  return fn;
}
