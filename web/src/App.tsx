import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Navigate, Route, BrowserRouter as Router, Routes } from "react-router-dom";
import { Layout } from "@/components/Layout";
import { Toaster } from "@/components/Toaster";
import { EventInvalidator } from "@/lib/eventInvalidator";
import { DeviceTable } from "@/views/DeviceTable";
import { NetworkList } from "@/views/NetworkList";

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
            <Route index element={<Navigate to="/networks" replace />} />
            <Route path="/networks" element={<NetworkList />} />
            <Route path="/networks/:nid" element={<DeviceTable />} />
            <Route path="*" element={<Navigate to="/networks" replace />} />
          </Route>
        </Routes>
      </Router>
      <Toaster />
    </QueryClientProvider>
  );
}
