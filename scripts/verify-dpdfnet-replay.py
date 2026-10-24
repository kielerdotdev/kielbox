"""Check the short raw-recording + cleaned-replay proof (history starts at 0).
No audio is played or uploaded. FFmpeg/FFprobe are needed for independent checks.
"""
import array
import json
import math
import os
from pathlib import Path
import subprocess
import sys


def run(tool,*args):
    return subprocess.run([os.environ.get(f'RCR_{tool.upper()}',tool),*map(str,args)],check=True,
                          stdout=subprocess.PIPE,stderr=subprocess.PIPE).stdout


def metadata(path):
    return json.loads(run('ffprobe','-v','error','-show_streams','-show_format','-of','json',path))


def video_packets(path):
    return json.loads(run('ffprobe','-v','error','-select_streams','v','-show_packets','-show_data_hash','sha256',
                          '-show_entries','packet=pts_time,data_hash,flags','-of','json',path))['packets']


def audio(path,index):
    return run('ffmpeg','-v','error','-threads','2','-i',path,'-map',f'0:{index}','-vn','-ac','2','-ar','48000','-f','f32le','-')


def main(raw,clean):
    a,b=metadata(raw),metadata(clean)
    assert len(a['streams'])==len(b['streams'])==3, 'Expected video/system/microphone streams'
    assert b['streams'][2]['tags']['title']=='Microphone (DPDFNet)'
    first,second=video_packets(raw),video_packets(clean)
    assert float(second[0]['pts_time'])==0 and 'K' in second[0]['flags'], 'This proof expects history starting at 0'
    assert [(p['pts_time'],p['data_hash']) for p in first[:len(second)]]==[(p['pts_time'],p['data_hash']) for p in second], 'Video was changed'
    raw_system,clean_system=audio(raw,1),audio(clean,1)
    assert clean_system and raw_system[:len(clean_system)]==clean_system, 'System PCM was changed'
    raw_mic,clean_mic=audio(raw,2),audio(clean,2)
    assert clean_mic and len(clean_mic)<=len(raw_mic) and len(clean_mic)%8==0
    x,y=array.array('f'),array.array('f');x.frombytes(raw_mic[:len(clean_mic)]);y.frombytes(clean_mic)
    if sys.byteorder!='little':x.byteswap();y.byteswap()
    assert all(map(math.isfinite,y)), 'Non-finite cleaned microphone'
    assert all(y[i]==y[i+1] for i in range(0,len(y),2)), 'Expected dual-mono microphone output'
    # Packet boundaries/PTS survive the processor; only the snapshot's final
    # packet can be shortened to meet the last retained video frame.
    def mic_packets(path):
        return json.loads(run('ffprobe','-v','error','-select_streams','a:1','-show_packets','-show_entries',
                              'packet=pts_time,size','-of','json',path))['packets']
    before,after=mic_packets(raw),mic_packets(clean)
    assert len(after)<=len(before)
    for i,p in enumerate(after):
        assert p['pts_time']==before[i]['pts_time'], 'Microphone timestamp changed'
        if i<len(after)-1:assert p['size']==before[i]['size'], 'Microphone packet size changed'
    for path in (raw,clean):run('ffmpeg','-v','error','-threads','2','-i',path,'-map','0','-f','null','-')
    difference=sum(v!=w for v,w in zip(x,y))
    rms=lambda values: math.sqrt(sum(v*v for v in values)/len(values))
    print(f'Unchanged AV1 packets: {len(second)}; unchanged system PCM bytes: {len(clean_system)}')
    print(f'Microphone frames: {len(clean_mic)//8}; changed channel samples: {difference}/{len(y)}; RMS raw={rms(x):.7f} cleaned={rms(y):.7f}')
    print('Verified full decode, dual-mono output and retained microphone packet timestamps/sample lengths.')


if __name__=='__main__':
    if len(sys.argv)!=3:raise SystemExit('Usage: python scripts/verify-dpdfnet-replay.py RAW.mkv CLEAN-REPLAY.mkv')
    main(Path(sys.argv[1]),Path(sys.argv[2]))
