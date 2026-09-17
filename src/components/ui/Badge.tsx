import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "@/lib/utils";

const badgeVariants = cva(
  "inline-flex items-center gap-1 rounded-full border border-transparent px-2 py-0.5 text-xs font-medium leading-5",
  {
    variants: {
      variant: {
        neutral: "bg-surface-hover text-text-secondary",
        success: "border-success/20 bg-success/12 text-success",
        warning: "border-warning/20 bg-warning/12 text-warning",
        danger: "border-danger/20 bg-danger/12 text-danger",
        info: "border-info/20 bg-info/12 text-info",
        primary: "border-primary/20 bg-primary/12 text-primary",
        route: "border-route/20 bg-route/12 text-route",
        outline: "border-border-strong/70 bg-transparent text-text-secondary",
      },
    },
    defaultVariants: { variant: "neutral" },
  }
);

export interface BadgeProps
  extends React.HTMLAttributes<HTMLSpanElement>,
    VariantProps<typeof badgeVariants> {}

export function Badge({ className, variant, ...props }: BadgeProps) {
  return (
    <span className={cn(badgeVariants({ variant }), className)} {...props} />
  );
}
