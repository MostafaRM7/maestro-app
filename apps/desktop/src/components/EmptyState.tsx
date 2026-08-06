import type { ReactNode } from "react";

interface EmptyStateProps {
  action?: ReactNode;
  description: string;
  eyebrow?: string;
  icon?: ReactNode;
  title: string;
}

export function EmptyState({ action, description, eyebrow, icon, title }: EmptyStateProps) {
  return (
    <section className="empty-state" aria-labelledby="empty-state-title">
      {icon ? <div className="empty-state__icon">{icon}</div> : null}
      {eyebrow ? <p className="eyebrow">{eyebrow}</p> : null}
      <h1 id="empty-state-title">{title}</h1>
      <p>{description}</p>
      {action ? <div className="empty-state__actions">{action}</div> : null}
    </section>
  );
}
