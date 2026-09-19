#!/usr/bin/env python3
"""Load the packaged plugin into a tiny X11 host, with no Chartr/GPUI dependency.
Checks shared helper ownership, playback, two panes, renderer recovery and teardown.
Run under Xvfb in CI or an X11 desktop; no system settings are changed.
"""
import ctypes as c
import http.server
import json
import os
from pathlib import Path
import signal
import sys
import tempfile
import threading
import time

package = Path(sys.argv[1]).resolve()
class Bytes(c.Structure):
    _fields_ = [('data', c.c_void_p), ('len', c.c_size_t)]
Emit = c.CFUNCTYPE(None, c.c_void_p, Bytes)
class Host(c.Structure):
    _fields_ = [('context', c.c_void_p), ('emit', Emit)]
class Create(c.Structure):
    _fields_ = [('abi', c.c_uint32), ('size', c.c_uint32), ('parent_kind', c.c_uint32), ('parent', c.c_size_t), ('package_dir', c.c_char_p), ('data_dir', c.c_char_p), ('instance', Bytes), ('host', Host)]
Handle = c.c_void_p
class Api(c.Structure):
    _fields_ = [('abi', c.c_uint32), ('size', c.c_uint32),
        ('create', c.CFUNCTYPE(Handle, c.POINTER(Create))),
        ('destroy', c.CFUNCTYPE(None, Handle)),
        ('resize', c.CFUNCTYPE(None, Handle, *([c.c_double] * 5))),
        ('visible', c.CFUNCTYPE(None, Handle, c.c_bool)),
        ('focus', c.CFUNCTYPE(None, Handle, c.c_bool)),
        ('dispatch', c.CFUNCTYPE(None, Handle, Bytes)),
        ('shutdown', c.CFUNCTYPE(None))]

def descendants(pid):
    children = Path(f'/proc/{pid}/task/{pid}/children')
    result = set()
    try:
        for child in map(int, children.read_text().split()):
            result.add(child)
            result.update(descendants(child))
    except FileNotFoundError:
        pass
    return result

def controllers():
    return {pid for pid in descendants(os.getpid()) if b'--serve' in Path(f'/proc/{pid}/cmdline').read_bytes().split(b'\0')}

video = (Path(__file__).parent / 'fixtures/media.webm').read_bytes()
class Page(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == '/media.webm':
            body, kind = video, 'video/webm'
        else:
            # Title only changes after real video frames played to completion.
            body = b'''<title>Loading media</title><video autoplay muted src="/media.webm"></video><script>
let v=document.querySelector('video');
v.onended=()=>{document.title=v.getVideoPlaybackQuality().totalVideoFrames>0?'Playback passed':'No frames';window.dispatchEvent(new KeyboardEvent('keydown',{key:'l',ctrlKey:true}));};
v.onerror=()=>document.title='Playback error';
</script>'''
            kind = 'text/html'
        self.send_response(200)
        self.send_header('Content-Type', kind)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        try:
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass
    def log_message(self, *_):
        pass
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Page)
threading.Thread(target=server.serve_forever, daemon=True).start()
url = f'http://127.0.0.1:{server.server_port}/'
x = c.CDLL('libX11.so.6')
x.XInitThreads()
x.XOpenDisplay.restype = Handle
x.XDefaultRootWindow.argtypes = [Handle]; x.XDefaultRootWindow.restype = c.c_ulong
x.XCreateSimpleWindow.argtypes = [Handle, c.c_ulong, c.c_int, c.c_int, c.c_uint, c.c_uint, c.c_uint, c.c_ulong, c.c_ulong]
x.XCreateSimpleWindow.restype = c.c_ulong
for fn in ('XMapWindow', 'XDestroyWindow'):
    getattr(x, fn).argtypes = [Handle, c.c_ulong]
x.XFlush.argtypes = [Handle]
x.XCloseDisplay.argtypes = [Handle]
display = x.XOpenDisplay(None)
assert display, 'X11 display required (use xvfb-run)'
parent = x.XCreateSimpleWindow(display, x.XDefaultRootWindow(display), 0, 0, 800, 600, 0, 0, 0)
x.XMapWindow(display, parent); x.XFlush(display)
library = c.CDLL(str(package / 'libchartr_browser.so'))
library.chartr_native_plugin_v1.restype = c.POINTER(Api)
api = library.chartr_native_plugin_v1().contents
assert api.abi == 1 and api.size >= c.sizeof(Api)
panes = {}
late = []
@Emit
def emit(context, data):
    event = json.loads(c.string_at(data.data, data.len))
    if context in panes:
        panes[context]['events'].append(event)
    else:
        late.append((context, event))
def dispatch(handle, value):
    data = json.dumps(value).encode()
    buf = c.create_string_buffer(data)
    api.dispatch(handle, Bytes(c.cast(buf, Handle), len(data)))
def until(check):
    deadline = time.monotonic() + 40
    while True:
        for pane in list(panes.values()):
            events, pane['events'] = pane['events'], []
            for event in events:
                if event['type'] == 'wake':
                    dispatch(pane['handle'], {'type': 'poll'})
                elif event['type'] == 'state':
                    pane['title'] = event['title']
                    pane['controls'] = event['controls']
                elif event['type'] == 'error':
                    raise AssertionError(event['message'])
                elif event['type'] == 'focus':
                    pane['ipc'] = event.get('control') == 'location'
        if check():
            return
        assert time.monotonic() < deadline, f'Timed out: {panes}'
        time.sleep(.01)
with tempfile.TemporaryDirectory(prefix='chartr-browser-test-') as data:
    def create(number):
        instance = json.dumps({'space':'test','instance_id':number,'theme':{'page':'#202020','field':'#303030','border':'#555555','focus':'#4488ff','text':'#ffffff','muted':'#aaaaaa','uiFontSize':'14px'}}).encode()
        buf = c.create_string_buffer(instance)
        options = Create(1, c.sizeof(Create), 1, parent, os.fsencode(package), os.fsencode(data), Bytes(c.cast(buf, Handle), len(instance)), Host(number, emit))
        pane = panes[number] = {'events': [], 'title': '', 'ipc': False}
        pane['handle'] = api.create(c.byref(options))
        assert pane['handle'], pane
        api.resize(pane['handle'], 0, 0, 400, 300, 1)
        api.visible(pane['handle'], True)
        dispatch(pane['handle'], {'type':'action','id':'navigate','value':url})
        return pane
    def destroy(number):
        pane = panes[number]
        api.destroy(pane['handle'])
        del panes[number]
    try:
        first = create(1)
        until(lambda: first['title'] == 'Playback passed' and first['ipc'])
        print('First pane playback and IPC passed', flush=True)
        original = controllers()
        assert len(original) == 1, original
        assert 'libcef.so' not in Path('/proc/self/maps').read_text()
        second = create(2)
        until(lambda: second['title'] == 'Playback passed' and second['ipc'])
        assert controllers() == original, 'Each pane spawned a new engine'
        print('Two panes share one helper', flush=True)
        destroy(1)
        dispatch(second['handle'], {'type':'action','id':'reload'})
        second['title'] = ''
        until(lambda: second['title'] == 'Playback passed')
        assert controllers() == original, 'Closing a sibling stopped the shared engine'
        print('Sibling close and surviving pane playback passed', flush=True)
        # A renderer has a compositor thread; GPU and control processes do not.
        renderers = [pid for pid in descendants(next(iter(original)))
            if any(task.read_text().strip() == 'Compositor' for task in Path(f'/proc/{pid}/task').glob('*/comm'))]
        assert renderers, 'No renderer to exercise crash recovery'
        for renderer in renderers:
            os.kill(renderer, signal.SIGKILL)
        until(lambda: second['title'] == 'Browser')
        dispatch(second['handle'], {'type':'action','id':'reload'})
        until(lambda: second['title'] == 'Playback passed')
        print('Renderer recovery passed', flush=True)
        destroy(2)
        until(lambda: not controllers())
        third = create(3)
        until(lambda: third['title'] == 'Playback passed')
        assert controllers() and controllers() != original
        destroy(3)
        until(lambda: not controllers())
        assert not late, f'Callbacks after destroy: {late}'
        print('PASS: playback, IPC, shared engine, isolated close, renderer recovery, last-close/reopen; no CEF in host')
    finally:
        for number in list(panes):
            destroy(number)
        api.shutdown()
        x.XDestroyWindow(display, parent)
        x.XCloseDisplay(display)
        server.shutdown()
