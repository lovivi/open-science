import { useTranslation } from "react-i18next";
import { Link } from "react-router-dom";
import { EmptyState } from "@/components/cards/EmptyState";

export function NotFound() {
  const { t } = useTranslation();
  return (
    <div className="flex h-full flex-col items-center justify-center">
      <EmptyState title={t("errors.notFound")} hint="This page does not exist." />
      <Link to="/" className="text-sm text-link underline underline-offset-2">
        {t("errors.goHome")}
      </Link>
    </div>
  );
}
