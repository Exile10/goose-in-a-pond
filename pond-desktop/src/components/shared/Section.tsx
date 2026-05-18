import { Card, CardContent } from "@heroui/react";
import type { ReactNode } from "react";

export function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <Card className="card">
      <CardContent>
        <h3 className="section__title">{title}</h3>
        <div className="section__rows">{children}</div>
      </CardContent>
    </Card>
  );
}
