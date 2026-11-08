// Test-only native UIA client. Read only the focused control of the owned GUI.
// Keep COM patterns/ranges alive explicitly; no .NET RawTextRange bridge.
#include <windows.h>
#include <UIAutomation.h>
#include <wrl/client.h>
#include <cstdio>
#include <cstdlib>
#include <cwchar>
#include <stdexcept>
using Microsoft::WRL::ComPtr;
static void check(HRESULT hr) { if (FAILED(hr)) throw hr; }
struct String {
    BSTR value = nullptr;
    ~String() { SysFreeString(value); }
};
static void json(BSTR text) {
    std::printf("\"");
    for (UINT i=0; i<SysStringLen(text); ++i) std::printf("\\u%04x", unsigned(text[i]));
    std::printf("\"");
}
int wmain(int argc, wchar_t** argv) {
    if (argc != 2) return 2;
    const DWORD owned = std::wcstoul(argv[1], nullptr, 10);
    if (!owned) return 2;
    HRESULT initialized = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
    if (FAILED(initialized)) return 3;
    int result = 0;
    try {
        ComPtr<IUIAutomation> automation;
        check(CoCreateInstance(CLSID_CUIAutomation, nullptr, CLSCTX_INPROC_SERVER,
                               IID_PPV_ARGS(automation.GetAddressOf())));
        ComPtr<IUIAutomationElement> edit;
        check(automation->GetFocusedElement(&edit));
        if (!edit) throw E_FAIL;
        int process = 0;
        check(edit->get_CurrentProcessId(&process));
        String name;
        check(edit->get_CurrentName(&name.value));
        if (DWORD(process) != owned || !name.value || std::wcscmp(name.value, L"Search results"))
            throw E_ACCESSDENIED;
        ComPtr<IUIAutomationValuePattern> value;
        check(edit->GetCurrentPatternAs(UIA_ValuePatternId, IID_PPV_ARGS(value.GetAddressOf())));
        String current;
        check(value->get_CurrentValue(&current.value));
        ComPtr<IUIAutomationTextPattern> pattern;
        check(edit->GetCurrentPatternAs(UIA_TextPatternId, IID_PPV_ARGS(pattern.GetAddressOf())));
        ComPtr<IUIAutomationTextRangeArray> ranges;
        check(pattern->GetSelection(&ranges));
        if (!ranges) throw E_FAIL;
        int count = 0;
        check(ranges->get_Length(&count));
        if (count != 1) throw E_FAIL;
        ComPtr<IUIAutomationTextRange> range;
        check(ranges->GetElement(0, &range));
        String selected;
        check(range->GetText(-1, &selected.value));
        int endpoints = 0;
        check(range->CompareEndpoints(TextPatternRangeEndpoint_Start, range.Get(),
                                      TextPatternRangeEndpoint_End, &endpoints));
        std::printf("{\"value\":"); json(current.value);
        std::printf(",\"selected_text\":"); json(selected.value);
        std::printf(",\"selection_ranges\":%d,\"collapsed\":%s,\"process_id\":%lu}\n",
                    count, endpoints == 0 ? "true" : "false", owned);
    } catch (HRESULT hr) {
        std::fprintf(stderr, "Owned native UIA selection failed: 0x%08lx\n", static_cast<unsigned long>(hr));
        result = 1;
    }
    CoUninitialize();
    return result;
}
