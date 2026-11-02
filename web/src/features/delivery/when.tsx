import { useFormat } from "./format";

export function When({ iso }: { iso: string }) {
  const format = useFormat();
  return (
    <time dateTime={iso} title={format.absolute(iso)}>
      {format.relative(iso)}
    </time>
  );
}
