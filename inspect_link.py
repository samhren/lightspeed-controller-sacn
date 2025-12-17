
import aalink
import asyncio

loop = asyncio.new_event_loop()
try:
    link = aalink.Link(120.0, loop)
    link.enabled = True
    print(dir(link))
    try:
        if hasattr(link, 'captureAppSessionState'):
            print("Has captureAppSessionState")
        if hasattr(link, 'capture_app_session_state'):
            print("Has capture_app_session_state")
        if hasattr(link, 'captureAudioSessionState'):
            print("Has captureAudioSessionState")
        if hasattr(link, 'capture_audio_session_state'):
            print("Has capture_audio_session_state")
    except Exception as e:
        print(e)
finally:
    loop.close()
