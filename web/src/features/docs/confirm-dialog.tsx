import { useState } from "react";

import { Button } from "@/ui/button";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogTitle,
  DialogTrigger,
} from "@/ui/dialog";

type Controlled = { open: boolean; onCancel: () => void; trigger?: never };
type Triggered = { trigger: string; open?: never; onCancel?: never };

export function ConfirmDialog(
  props: {
    title: string;
    body: string;
    cancel: string;
    confirm: string;
    onConfirm: () => void;
  } & (Controlled | Triggered),
) {
  const [own, setOwn] = useState(false);
  const controlled = props.trigger === undefined;
  const open = controlled ? props.open : own;
  const change = (next: boolean) => {
    if (controlled) {
      if (!next) {
        props.onCancel();
      }
    } else {
      setOwn(next);
    }
  };
  return (
    <Dialog open={open} onOpenChange={change}>
      {!controlled && (
        <DialogTrigger asChild>
          <Button variant="outline">{props.trigger}</Button>
        </DialogTrigger>
      )}
      <DialogContent>
        <DialogTitle>{props.title}</DialogTitle>
        <DialogDescription>{props.body}</DialogDescription>
        <div className="flex flex-wrap justify-end gap-2">
          <DialogClose asChild>
            <Button variant="outline">{props.cancel}</Button>
          </DialogClose>
          <Button
            onClick={() => {
              if (!controlled) {
                setOwn(false);
              }
              props.onConfirm();
            }}
          >
            {props.confirm}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
