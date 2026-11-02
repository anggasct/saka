import { useQuery } from "@tanstack/react-query";

import { loadSpecTools } from "./spec-tools";

export function useSpecTools() {
  return useQuery({
    queryKey: ["spec-tools"],
    queryFn: loadSpecTools,
    staleTime: Infinity,
    gcTime: Infinity,
  });
}
