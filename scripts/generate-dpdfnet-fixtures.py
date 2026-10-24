"""Development-only independent ONNX reference; never imported by the app.

Requires numpy/onnxruntime. Produces deterministic spectra, recurrent-state and
latency-compensated PCM fixtures for the pure-Rust implementation.
"""
import hashlib
from pathlib import Path
import sys
import numpy as np
import onnxruntime as ort

ROOT=Path(__file__).resolve().parents[1]
path=Path(sys.argv[1]) if len(sys.argv)>1 else ROOT/'.downloads/dpdfnet2_48khz_hr.onnx'
assert hashlib.sha256(path.read_bytes()).hexdigest()=='7f0575a5cec0ba4ffd8f8bd657e06d007e4ccdd955d76faab922b9d3291dc14b'
options=ort.SessionOptions();options.intra_op_num_threads=1;options.inter_op_num_threads=1
session=ort.InferenceSession(str(path),sess_options=options,providers=['CPUExecutionProvider'])
meta=session.get_modelmeta().custom_metadata_map
state=np.zeros(int(meta['state_size']),np.float32)
erb=np.array(meta['erb_norm_init'].split(','),np.float32);spec=np.array(meta['spec_norm_init'].split(','),np.float32)
state[:len(erb)]=erb;state[len(erb):len(erb)+len(spec)]=spec
hop=480;fft=960
t=np.arange(fft,dtype=np.float32)
window=np.sin(np.float32(np.pi/2)*np.sin(np.float32(np.pi)*(t+np.float32(.5))/np.float32(fft))**2).astype(np.float32)
# Reproducible, public-domain synthetic signal; no user recordings in fixtures.
seed=123456789;noise=[]
for _ in range(48000):
    seed=(1664525*seed+1013904223)&0xffffffff
    noise.append((seed/4294967296-.5)*.06)
samples=np.array(noise,np.float32)
time=np.arange(48000)/48000
samples+=(.12*np.sin(2*np.pi*190*time)+.04*np.sin(2*np.pi*380*time)).astype(np.float32)*(time>.25)*(time<.75)
ring=np.zeros(fft,np.float32);ola=np.zeros(fft,np.float32)
inputs=[];outputs=[];audio=[];gold_state=None
for index in range(105):
    block=samples[index*hop:(index+1)*hop] if index<100 else np.zeros(hop,np.float32)
    ring[:hop]=ring[hop:];ring[hop:]=block
    spectrum=np.fft.rfft(ring*window).astype(np.complex64)
    ri=np.stack((spectrum.real,spectrum.imag),axis=-1).astype(np.float32)
    enhanced,state=session.run(None,{'spec':ri[None,None,:,:],'state_in':state})
    if index<12:inputs.append(ri.copy());outputs.append(enhanced.flatten())
    if index==11:gold_state=state.copy()
    e=enhanced[0,0];frame=(np.fft.irfft(e[:,0]+1j*e[:,1],n=fft)*window).astype(np.float32)
    ola[:hop]=ola[hop:];ola[hop:]=0;ola+=frame
    audio.extend(ola[:hop].copy())
destination=ROOT/'crates/dpdfnet/tests/fixtures';destination.mkdir(parents=True,exist_ok=True)
for name,value in [('model-inputs',inputs),('model-outputs',outputs),('model-final-state',gold_state),('audio-input',samples),('audio-output',audio[2400:2400+48000])]:
    data=np.array(value,dtype='<f4').tobytes();(destination/f'{name}.f32').write_bytes(data)
    print(name,len(data),hashlib.sha256(data).hexdigest())
