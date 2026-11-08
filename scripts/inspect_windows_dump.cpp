// Diagnostic-only reader for minidumps of the GUI process owned by our test.
#include <windows.h>
#include <dbgeng.h>
#include <cstdio>

struct DebuggerOutput final : IDebugOutputCallbacks {
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID id, void** out) override {
        if (id == __uuidof(IUnknown) || id == __uuidof(IDebugOutputCallbacks)) {
            *out = this;
            return S_OK;
        }
        *out = nullptr;
        return E_NOINTERFACE;
    }
    ULONG STDMETHODCALLTYPE AddRef() override { return 1; }
    ULONG STDMETHODCALLTYPE Release() override { return 1; }
    HRESULT STDMETHODCALLTYPE Output(ULONG, PCSTR text) override {
        std::fputs(text, stdout);
        return S_OK;
    }
};

int main(int argc, char** argv) {
    if (argc != 3) return 2;
    IDebugClient* client = nullptr;
    IDebugControl* control = nullptr;
    IDebugSymbols* symbols = nullptr;
    DebuggerOutput output;
    HRESULT hr = DebugCreate(__uuidof(IDebugClient), reinterpret_cast<void**>(&client));
    if (FAILED(hr)) { std::printf("DebugCreate: %08lx\n", hr); return 1; }
    client->SetOutputCallbacks(&output);
    hr = client->QueryInterface(__uuidof(IDebugControl), reinterpret_cast<void**>(&control));
    if (SUCCEEDED(hr)) hr = client->QueryInterface(__uuidof(IDebugSymbols), reinterpret_cast<void**>(&symbols));
    if (SUCCEEDED(hr)) hr = symbols->SetSymbolPath(argv[2]);
    if (SUCCEEDED(hr)) hr = client->OpenDumpFile(argv[1]);
    if (SUCCEEDED(hr)) hr = control->WaitForEvent(0, INFINITE);
    if (SUCCEEDED(hr)) hr = control->Execute(DEBUG_OUTCTL_THIS_CLIENT, "lm; ~* kb 50", DEBUG_EXECUTE_DEFAULT);
    std::printf("dump inspection result: %08lx\n", hr);
    client->EndSession(DEBUG_END_PASSIVE);
    if (symbols) symbols->Release();
    if (control) control->Release();
    client->SetOutputCallbacks(nullptr);
    client->Release();
    return FAILED(hr) ? 1 : 0;
}
