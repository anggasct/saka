import { useId } from "react";
import type { ReactNode } from "react";

import { cn } from "./cn";

export type RadioOption = {
  value: string;
  label: ReactNode;
  description?: ReactNode;
};

export function RadioGroup({
  legend,
  name,
  value,
  options,
  onChange,
  className,
}: {
  legend: ReactNode;
  name: string;
  value: string;
  options: readonly RadioOption[];
  onChange: (value: string) => void;
  className?: string;
}) {
  const prefix = useId();
  return (
    <fieldset className={cn("space-y-1", className)}>
      <legend className="text-sm font-medium">{legend}</legend>
      {options.map((option) => {
        const id = `${prefix}-${option.value}`;
        const hint = `${id}-hint`;
        return (
          <div key={option.value} className="flex items-start gap-3">
            <input
              id={id}
              type="radio"
              name={name}
              value={option.value}
              checked={value === option.value}
              aria-describedby={
                option.description === undefined ? undefined : hint
              }
              className="mt-3 size-5 shrink-0 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring"
              onChange={() => {
                onChange(option.value);
              }}
            />
            <label htmlFor={id} className="min-h-11 flex-1 py-2 text-sm">
              <span className="font-medium">{option.label}</span>
              {option.description !== undefined && (
                <span id={hint} className="block text-muted-foreground">
                  {option.description}
                </span>
              )}
            </label>
          </div>
        );
      })}
    </fieldset>
  );
}
