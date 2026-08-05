import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";
import { Link } from "react-router-dom";
import { cn, externalLinkProps } from "@/lib/utils";

/**
 * 按钮样式，与桌面应用 src/components/ui/Button.tsx 同源，
 * 补了官网需要的 `xl` 尺寸（Hero 主按钮）。
 *
 * 移动端触控目标不小于 44px 是模板第 9 节的硬要求：md 以上尺寸都 ≥ 44px，
 * sm 只用在桌面端的次要位置。
 */
const buttonVariants = cva(
  "inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-control font-medium transition-all focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background disabled:pointer-events-none disabled:opacity-50 select-none",
  {
    variants: {
      variant: {
        primary: "bg-primary text-primary-foreground shadow-card hover:opacity-90 active:scale-[0.99]",
        secondary: "bg-surface-hover text-text-primary border border-border hover:bg-border/40",
        outline: "border border-border bg-transparent text-text-primary hover:bg-surface-hover",
        ghost: "text-text-secondary hover:bg-surface-hover hover:text-text-primary",
      },
      size: {
        sm: "h-8 px-3 text-xs",
        md: "h-11 px-4 text-sm",
        lg: "h-12 px-6 text-[15px]",
        xl: "h-14 px-7 text-base",
        icon: "h-11 w-11",
      },
    },
    defaultVariants: { variant: "primary", size: "md" },
  }
);

type BaseProps = VariantProps<typeof buttonVariants> & { className?: string };

export interface ButtonProps
  extends React.ButtonHTMLAttributes<HTMLButtonElement>,
    BaseProps {}

export const Button = React.forwardRef<HTMLButtonElement, ButtonProps>(
  ({ className, variant, size, ...props }, ref) => (
    <button ref={ref} className={cn(buttonVariants({ variant, size }), className)} {...props} />
  )
);
Button.displayName = "Button";

/** 站内跳转按钮 */
export function ButtonLink({
  to,
  className,
  variant,
  size,
  children,
  ...props
}: BaseProps & { to: string; children: React.ReactNode } & Omit<
    React.ComponentProps<typeof Link>,
    "to" | "className"
  >) {
  return (
    <Link to={to} className={cn(buttonVariants({ variant, size }), className)} {...props}>
      {children}
    </Link>
  );
}

/** 外链按钮：统一带 noopener noreferrer */
export function ButtonExternal({
  href,
  className,
  variant,
  size,
  children,
  ...props
}: BaseProps & { href: string; children: React.ReactNode } & Omit<
    React.AnchorHTMLAttributes<HTMLAnchorElement>,
    "href" | "className"
  >) {
  return (
    <a href={href} className={cn(buttonVariants({ variant, size }), className)} {...externalLinkProps} {...props}>
      {children}
    </a>
  );
}

export { buttonVariants };
