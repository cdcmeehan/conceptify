import { useEffect, useState } from "preact/hooks";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./App.css";

function App() {
  const [greetMsg, setGreetMsg] = useState("");
  const [name, setName] = useState("");

  async function greet() {
    // Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
    setGreetMsg(await invoke("greet", { name }));
  }

  useEffect(() => {
    // Demo listener for the local API's event bridge (PRD §5.1): the axum
    // handler behind `GET /api/v1/ping` emits an `api-ping` Tauri event,
    // which flows here just like real mutation events (`artifact-updated`,
    // `comment-resolved`, ...) will once those routes exist.
    const unlisten = listen<{ message: string; unix_ms: number }>(
      "api-ping",
      (event) => {
        console.log("[conceptify] received api-ping event:", event.payload);
      },
    );
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  return (
    <main className="container bg-gray-100 p-8 rounded-lg">
      <h1 className="text-4xl font-bold text-blue-600 mb-6">Welcome to Conceptify</h1>

      <div className="row">
        <a href="https://vite.dev" target="_blank">
          <img src="/vite.svg" className="logo vite" alt="Vite logo" />
        </a>
        <a href="https://tauri.app" target="_blank">
          <img src="/tauri.svg" className="logo tauri" alt="Tauri logo" />
        </a>
        <a href="https://preactjs.com" target="_blank">
          <svg className="logo preact" viewBox="0 0 256 296" xmlns="http://www.w3.org/2000/svg" width="64" height="64">
            <path fill="#673AB8" d="m128 0l128 73.9v147.8l-128 73.9L0 221.7V73.9z"/>
            <ellipse cx="128" cy="147.8" fill="none" stroke="#FFF" stroke-width="16" rx="71.5" ry="116.5"/>
            <ellipse cx="128" cy="147.8" fill="none" stroke="#FFF" stroke-width="16" rx="71.5" ry="116.5" transform="rotate(60 128 147.8)"/>
            <ellipse cx="128" cy="147.8" fill="none" stroke="#FFF" stroke-width="16" rx="71.5" ry="116.5" transform="rotate(120 128 147.8)"/>
            <circle cx="128" cy="147.8" fill="#FFF" r="18"/>
          </svg>
        </a>
      </div>
      <p className="text-gray-600">Click on the Tauri, Vite, and Preact logos to learn more.</p>

      <form
        className="row"
        onSubmit={(e) => {
          e.preventDefault();
          greet();
        }}
      >
        <input
          id="greet-input"
          onChange={(e) => setName(e.currentTarget.value)}
          placeholder="Enter a name..."
          className="px-4 py-2 border border-gray-300 rounded"
        />
        <button type="submit" className="px-6 py-2 bg-blue-600 text-white rounded hover:bg-blue-700 transition">Greet</button>
      </form>
      <p className="text-green-600 font-semibold mt-4">{greetMsg}</p>
    </main>
  );
}

export default App;
