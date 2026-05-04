import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Navigate, Route, BrowserRouter as Router, Routes } from "react-router-dom";
import { Layout } from "@/components/Layout";
import { Toaster } from "@/components/Toaster";
import { EventInvalidator } from "@/lib/eventInvalidator";
import { UnifiedView } from "@/views/UnifiedView";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: 1,
      staleTime: 2000,
      refetchOnWindowFocus: false,
    },
  },
});

export default function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <EventInvalidator />
      <Router>
        <Routes>
          <Route element={<Layout />}>
            {/* Phase 3 — single unified page covers what the old
                /networks and /networks/:nid pages used to do. Old
                routes redirect home so existing bookmarks still work. */}
            <Route index element={<UnifiedView />} />
            <Route path="/networks" element={<Navigate to="/" replace />} />
            <Route
              path="/networks/:nid"
              element={<Navigate to="/" replace />}
            />
            <Route path="*" element={<Navigate to="/" replace />} />
          </Route>
        </Routes>
      </Router>
      <Toaster />
    </QueryClientProvider>
  );
}
