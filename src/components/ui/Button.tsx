import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "@/lib/utils";

const buttonVariants = cva(
  "inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-control text-sm font-medium shadow-sm transition-all duration-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1 focus-visible:ring-offset-background active:scale-[0.98] disabled:pointer-events-none disabled:opacity-50 select-none",
  {
    variants: {
      variant: {
        primary:
          "bg-primary text-primary-foreground shadow-primary/20 hover:bg-primary/90 hover:shadow-primary/30",
        route:
          "bg-route text-route-foreground shadow-route/20 hover:bg-route-deep hover:shadow-route/30",
        secondary:
          "border border-border bg-surface-elevated text-text-primary hover:border-border-strong hover:bg-surface-hover",
        subtle:
          "bg-surface-hover/70 text-text-primary hover:bg-surface-hover hover:shadow-none",
        ghost:
          "text-text-secondary hover:bg-surface-hover hover:text-text-primary hover:shadow-none",
        danger:
          "bg-danger text-white shadow-danger/20 hover:bg-danger/90 hover:shadow-danger/30",
        "danger-outline":
          "border border-danger/40 bg-transparent text-danger hover:border-danger/60 hover:bg-danger/8 hover:shadow-none",
        outline:
          "border border-border bg-transparent text-text-primary hover:border-border-strong hover:bg-surface-hover hover:shadow-none",
      },
      size: {
        sm: "h-8 px-3 text-xs",
        md: "h-9 px-4",
        lg: "h-10 px-5",
        icon: "h-9 w-9 p-0",
      },
    },
    defaultVariants: { variant: "primary", size: "md" },
  }
);

export interface ButtonProps
  extends React.ButtonHTMLAttributes<HTMLButtonElement>,
    VariantProps<typeof buttonVariants> {}

export const Button = React.forwardRef<HTMLButtonElement, ButtonProps>(
  ({ className, variant, size, ...props }, ref) => (
    <button
      ref={ref}
      className={cn(buttonVariants({ variant, size }), className)}
      {...props}
    />
  )
);
Button.displayName = "Button";

export interface IconButtonProps
  extends Omit<ButtonProps, "children" | "aria-label"> {
  label: string;
  icon: React.ReactNode;
}

export const IconButton = React.forwardRef<HTMLButtonElement, IconButtonProps>(
  ({ label, icon, ...props }, ref) => (
    <Button ref={ref} {...props} size="icon" type="button" aria-label={label}>
      {icon}
    </Button>
  ),
);
IconButton.displayName = "IconButton";
