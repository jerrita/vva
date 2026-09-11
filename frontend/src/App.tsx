import { useEffect, useState, useRef } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { Send, Plus, MessageSquare, Terminal, ChevronRight, Activity } from 'lucide-react';

interface SessionSummary {
  id: string;
  title?: string;
  current_model: string;
  turn_count: number;
}

interface MessagePart {
  Text?: { text: string };
  Reasoning?: { content: string };
  ToolCall?: { name: string, arguments: string };
  ToolResult?: { content: string };
  WebSearchCall?: any;
}

interface Message {
  role: string;
  parts: MessagePart[];
}

interface Turn {
  id: string;
  role: string;
  messages: Message[];
}

interface Session {
  id: string;
  title?: string;
  turns: Turn[];
}

const API_BASE = window.location.origin.includes('localhost') ? 'http://127.0.0.1:3000' : '';

function App() {
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [currentSessionId, setCurrentSessionId] = useState<string | null>(null);
  const [sessionData, setSessionData] = useState<Session | null>(null);
  const [prompt, setPrompt] = useState('');
  const [loading, setLoading] = useState(false);
  const [agentState, setAgentState] = useState<string>('');
  const [draft, setDraft] = useState<any[]>([]);

  const messagesEndRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    fetchSessions();
  }, []);

  useEffect(() => {
    if (currentSessionId) {
      fetchSessionDetails(currentSessionId);

      const evtSource = new EventSource(`${API_BASE}/api/sessions/${currentSessionId}/stream`);
      evtSource.onmessage = (event) => {
        try {
          const data = JSON.parse(event.data);
          setAgentState(data.state);
          setDraft(data.draft);
          if (data.state === "") {
            fetchSessionDetails(currentSessionId);
            fetchSessions();
          }
        } catch (e) {
          console.error(e);
        }
      };

      return () => evtSource.close();
    }
  }, [currentSessionId]);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [sessionData, draft, agentState]);

  const fetchSessions = async () => {
    const res = await fetch(`${API_BASE}/api/sessions`);
    if (res.ok) {
      setSessions(await res.json());
    }
  };

  const fetchSessionDetails = async (id: string) => {
    const res = await fetch(`${API_BASE}/api/sessions/${id}`);
    if (res.ok) {
      setSessionData(await res.json());
    }
  };

  const createSession = async () => {
    const res = await fetch(`${API_BASE}/api/sessions`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({}),
    });
    if (res.ok) {
      const id = await res.json();
      setCurrentSessionId(id);
      fetchSessions();
    }
  };

  const submitPrompt = async () => {
    if (!prompt.trim() || !currentSessionId) return;

    setLoading(true);
    const text = prompt;
    setPrompt('');

    // Optimistic UI update
    const tempTurn: Turn = {
      id: 'temp-' + Date.now(),
      role: 'user',
      messages: [{ role: 'user', parts: [{ Text: { text } }] }]
    };
    if (sessionData) {
      setSessionData({ ...sessionData, turns: [...sessionData.turns, tempTurn] });
    }

    await fetch(`${API_BASE}/api/sessions/${currentSessionId}/prompt`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ prompt: text }),
    });

    setLoading(false);
  };

  const renderMessagePart = (part: any, index: number) => {
    if (part.Text) {
      return (
        <div key={index} className="prose prose-invert prose-p:leading-relaxed max-w-none prose-pre:bg-gray-900 prose-pre:border prose-pre:border-gray-800">
          <ReactMarkdown remarkPlugins={[remarkGfm]}>{part.Text.text}</ReactMarkdown>
        </div>
      );
    } else if (part.Reasoning) {
      return (
        <details key={index} className="group my-3 cursor-pointer">
           <summary className="flex items-center text-xs font-medium text-purple-400 select-none hover:text-purple-300 transition-colors">
              <ChevronRight size={14} className="mr-1 group-open:rotate-90 transition-transform" />
              Thought Process
           </summary>
           <div className="mt-2 ml-2 pl-3 border-l-2 border-purple-500/30 text-gray-400 italic text-sm">
             <ReactMarkdown remarkPlugins={[remarkGfm]}>{part.Reasoning.content}</ReactMarkdown>
           </div>
        </details>
      );
    } else if (part.ToolCall) {
       return (
        <div key={index} className="my-3 rounded-lg overflow-hidden border border-emerald-900/50 bg-emerald-950/20 shadow-sm">
           <div className="bg-emerald-900/40 px-3 py-1.5 flex items-center text-emerald-400 text-xs font-medium uppercase tracking-wider">
              <Activity size={12} className="mr-2" /> Tool Execution
           </div>
           <div className="p-3 text-emerald-300 font-mono text-sm break-all">
              {part.ToolCall.name}(<span className="text-emerald-500">{part.ToolCall.arguments}</span>)
           </div>
        </div>
      );
    } else if (part.ToolResult) {
      return (
        <details key={index} className="group my-2 cursor-pointer">
           <summary className="flex items-center text-xs font-medium text-emerald-600/80 select-none hover:text-emerald-500 transition-colors">
              <ChevronRight size={14} className="mr-1 group-open:rotate-90 transition-transform" />
              Tool Output
           </summary>
           <div className="mt-2 bg-gray-950 p-3 rounded-md text-xs text-gray-500 font-mono overflow-x-auto max-h-48 border border-gray-800 shadow-inner">
             {part.ToolResult.content}
           </div>
        </details>
      );
    }
    return null;
  };

  return (
    <div className="flex h-screen bg-[#09090b] text-gray-200 antialiased selection:bg-blue-500/30">
      {/* Sidebar */}
      <div className="w-72 border-r border-gray-800 flex flex-col bg-[#09090b] shadow-xl z-10 shrink-0">
        <div className="p-5 border-b border-gray-800 flex justify-between items-center bg-[#18181b]">
          <h1 className="font-bold tracking-tight text-white flex items-center gap-2">
             <Terminal size={20} className="text-blue-500" />
             VVA Web
          </h1>
          <button
            onClick={createSession}
            className="p-1.5 bg-blue-600 hover:bg-blue-500 rounded-md text-white transition-all shadow hover:shadow-blue-500/20"
            title="New Session"
          >
            <Plus size={16} />
          </button>
        </div>

        <div className="px-4 py-3 text-xs font-semibold tracking-wider text-gray-500 uppercase">
           Recent Sessions
        </div>

        <div className="flex-1 overflow-y-auto px-3 space-y-1 pb-4">
          {sessions.map(s => (
            <div
              key={s.id}
              onClick={() => setCurrentSessionId(s.id)}
              className={`px-3 py-2.5 rounded-lg cursor-pointer flex items-center gap-3 text-sm transition-all duration-200 ${
                currentSessionId === s.id
                  ? 'bg-blue-600/10 text-blue-100 shadow-[inset_2px_0_0_0_#3b82f6]'
                  : 'hover:bg-gray-800/60 text-gray-400 hover:text-gray-300'
              }`}
            >
              <MessageSquare size={16} className={currentSessionId === s.id ? 'text-blue-500' : 'text-gray-500'} />
              <div className="flex-1 min-w-0">
                 <div className="truncate font-medium">{s.title || 'Untitled Session'}</div>
                 <div className="text-[10px] text-gray-500 flex justify-between mt-0.5">
                    <span className="truncate max-w-[80%]">{s.current_model}</span>
                    <span>{s.turn_count} turns</span>
                 </div>
              </div>
            </div>
          ))}
        </div>
      </div>

      {/* Main Content */}
      <div className="flex-1 flex flex-col min-w-0 bg-[#09090b] relative">
        {currentSessionId ? (
          <>
             {/* Header */}
             <div className="h-14 border-b border-gray-800/60 flex items-center px-6 bg-[#09090b]/80 backdrop-blur-md sticky top-0 z-10 shrink-0">
                <h2 className="font-medium text-gray-300">
                   {sessionData?.title || 'Untitled Session'}
                </h2>
             </div>

            <div className="flex-1 overflow-y-auto p-4 md:p-8 space-y-8 pb-32">
              {sessionData?.turns.map((turn, i) => (
                <div key={i} className={`flex flex-col max-w-4xl mx-auto w-full ${turn.role === 'user' ? 'items-end' : 'items-start'}`}>
                  <div className={`flex items-center gap-2 mb-2 px-1 ${turn.role === 'user' ? 'flex-row-reverse' : 'flex-row'}`}>
                     <div className={`w-8 h-8 rounded-full flex items-center justify-center text-[10px] font-bold ${
                        turn.role === 'user' ? 'bg-[#0d6efd] text-white' : 'bg-[#a855f7] text-white'
                     }`}>
                        {turn.role === 'user' ? 'U' : 'A'}
                     </div>
                     <span className="text-xs font-medium text-gray-500 capitalize">{turn.role}</span>
                  </div>

                  <div className={`p-5 rounded-2xl max-w-[90%] md:max-w-[85%] shadow-sm ${
                     turn.role === 'user'
                        ? 'bg-[#0d6efd] text-white rounded-2xl rounded-tr-sm shadow-[0_4px_14px_0_rgb(0,118,255,0.39)]'
                        : 'bg-[#18181b] border border-gray-800 text-gray-200 rounded-2xl rounded-tl-sm'
                  }`}>
                    {turn.messages.map((msg, j) => (
                      <div key={j}>
                        {msg.parts.map((part, k) => renderMessagePart(part, k))}
                      </div>
                    ))}
                  </div>
                </div>
              ))}

              {/* Active Draft */}
              {(agentState || draft.length > 0) && (
                <div className="flex flex-col max-w-4xl mx-auto w-full items-start">
                   <div className="flex items-center gap-2 mb-2 px-1 flex-row">
                     <div className="w-8 h-8 rounded-full flex items-center justify-center text-[10px] font-bold bg-purple-600 text-white shadow-[0_0_10px_rgba(147,51,234,0.5)]">
                        A
                     </div>
                     <span className="text-xs font-medium text-gray-500 capitalize flex items-center gap-2">
                        Assistant
                        {agentState && (
                           <span className="flex items-center text-blue-400 bg-blue-900/20 px-2 py-0.5 rounded-full text-[10px]">
                              <span className="w-1.5 h-1.5 bg-blue-500 rounded-full animate-pulse mr-1.5"></span>
                              {agentState}
                           </span>
                        )}
                     </span>
                  </div>
                   <div className="p-5 rounded-2xl max-w-[90%] md:max-w-[85%] shadow-sm bg-[#18181b] border border-blue-900/30 text-gray-200 rounded-tl-sm w-full">
                    {draft.map((part: any, k: number) => renderMessagePart(part, k))}
                    {agentState === "Thinking..." && draft.length === 0 && (
                       <div className="text-gray-500 animate-pulse italic text-sm">Formulating thought process...</div>
                    )}
                  </div>
                </div>
              )}
              <div ref={messagesEndRef} className="h-1" />
            </div>

            {/* Input Area */}
            <div className="absolute bottom-0 w-full p-4 bg-gradient-to-t from-[#0a0a0a] via-[#0a0a0a] to-transparent pt-10">
              <div className="max-w-4xl mx-auto relative group">
                <textarea
                  value={prompt}
                  onChange={e => setPrompt(e.target.value)}
                  onKeyDown={e => {
                    if (e.key === 'Enter' && !e.shiftKey) {
                      e.preventDefault();
                      submitPrompt();
                    }
                  }}
                  disabled={loading || !!agentState}
                  placeholder="Message VVA... (Shift+Enter for new line)"
                  className="w-full bg-[#1a1a1f] border border-gray-700/60 rounded-2xl px-5 py-4 pr-14 focus:outline-none focus:border-blue-500 focus:ring-1 focus:ring-blue-500/50 resize-none min-h-[60px] max-h-48 text-gray-200 disabled:opacity-60 transition-all shadow-lg"
                  rows={1}
                />
                <button
                  onClick={submitPrompt}
                  disabled={!prompt.trim() || loading || !!agentState}
                  className="absolute right-3 bottom-3 p-2.5 bg-white text-black rounded-xl hover:bg-gray-200 hover:scale-105 disabled:opacity-30 disabled:hover:scale-100 disabled:hover:bg-white transition-all shadow-md flex items-center justify-center"
                >
                  <Send size={18} className={!prompt.trim() || loading || !!agentState ? '' : 'text-blue-600'} />
                </button>
              </div>
               <div className="text-center mt-2 text-[10px] text-gray-600 pb-1">
                  AI-generated content may be incorrect. Verify important information.
               </div>
            </div>
          </>
        ) : (
          <div className="flex-1 flex items-center justify-center text-gray-500 flex-col gap-5 bg-[radial-gradient(ellipse_at_center,_var(--tw-gradient-stops))] from-[#0a0a0a] to-[#0a0a0a]">
            <div className="p-6 rounded-full bg-[#18181b] border border-gray-800 shadow-2xl">
               <Terminal size={48} className="text-blue-500/50" />
            </div>
            <div className="text-center">
               <h2 className="text-xl font-medium text-gray-300 mb-2">Welcome to VVA Web</h2>
               <p className="text-sm max-w-sm mx-auto">Select a session from the sidebar or start a new one to begin chatting.</p>
            </div>
            <button
               onClick={createSession}
               className="mt-4 px-6 py-2.5 bg-blue-600 hover:bg-blue-500 text-white rounded-lg transition-all shadow-[0_0_15px_rgba(37,99,235,0.3)] font-medium text-sm flex items-center gap-2"
            >
               <Plus size={16} /> New Session
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

export default App;
