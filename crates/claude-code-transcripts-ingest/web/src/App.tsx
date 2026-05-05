import {
  BrowserRouter,
  Navigate,
  Route,
  Routes,
  useLocation,
  useParams,
} from "react-router-dom";
import { Layout } from "./routes/Layout";
import { DashboardPage } from "./routes/DashboardPage";
import { CostPage } from "./routes/CostPage";
import { SessionListPage } from "./routes/SessionListPage";
import { TranscriptPage } from "./routes/TranscriptPage";
import "./routes/SessionListPage.css";

function RedirectTranscript() {
  const { id = "" } = useParams<{ id: string }>();
  const { search } = useLocation();
  return <Navigate to={`/sessions/${encodeURIComponent(id)}${search}`} replace />;
}

export function App() {
  return (
    <BrowserRouter>
      <Routes>
        <Route element={<Layout />}>
          <Route path="/" element={<Navigate to="/dashboard" replace />} />
          <Route path="/dashboard" element={<DashboardPage />} />
          <Route path="/cost" element={<CostPage />} />
          <Route path="/sessions" element={<SessionListPage />} />
          <Route path="/sessions/:id" element={<TranscriptPage />} />
          <Route path="/transcripts" element={<Navigate to="/sessions" replace />} />
          <Route
            path="/transcripts/:id"
            element={<RedirectTranscript />}
          />
        </Route>
      </Routes>
    </BrowserRouter>
  );
}
