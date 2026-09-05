"""Prove the bandwidth detector against ground truth you construct, before trusting it.

Run this after touching `_audio_qa._bandwidth_hz`. It exists because the check it guards was wrong
once: the first implementation used a 95%-energy spectral rolloff, which measures where energy is
CONCENTRATED rather than how high content reaches, and a metric that plausible needs a test that
does not rely on judgement about real files.

The trick is that a lowpass built out of resampling has a cutoff known by construction: resample to
`target` Hz and back and the content cannot exceed `target/2`. So the expected answer is available
without any reference corpus, and the whole test is hermetic - a synthetic speech-like signal, no
audio files, no network.

    python bandwidth_selftest.py               # synthetic signal only
    python bandwidth_selftest.py --audio X.wav # also measure a real clip

Exit code is 0 only if every constructed cutoff is recovered within tolerance.
"""

from __future__ import annotations

import argparse
import pathlib
import sys

import numpy as np
import scipy.signal

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

from _audio_qa import _bandwidth_hz  # noqa: E402

RATE = 48000
#: Recovered cutoffs land ABOVE the constructed one, because the resampler's antialiasing filter has
#: a transition band and a finite stopband, and content there is still within 50 dB of the peak. The
#: bias is one-sided and was measured at +7% to +13% (4 -> 4.45, 8 -> 8.7, 16 -> 17.2 kHz here;
#: 4 -> 4.4, 8 -> 8.7, 16 -> 16.7 on real speech). 18% clears that with margin - a threshold that
#: only just passes today flakes on the next scipy - while still rejecting a factor-of-two miss.
TOLERANCE = 0.18


def speech_like(seconds: float = 6.0, rate: int = RATE) -> np.ndarray:
    """A signal shaped like speech: energy concentrated low, content all the way to Nyquist.

    Concentrated-low is what defeats a rolloff, and full-band is what a bandwidth detector must
    still see, so a signal with both properties is what separates the two measurements. Built as
    broadband noise with a speech-like spectral tilt rather than a harmonic stack: a harmonic series
    steep enough to look like a voice (-12 dB/octave) is ~168 dB down at Nyquist and genuinely has
    no high band, so the detector correctly reports 2.6 kHz for it and the test would fail for a
    real reason while teaching nothing about the detector.

    -4.5 dB/octave puts Nyquist about 36 dB below 100 Hz: unambiguously present to a -50 dB edge,
    while most of the energy still sits in the lowest few hundred Hz. An amplitude envelope plus
    leading and trailing silence exercise the speech-frame gate the detector measures through.
    """
    generator = np.random.default_rng(20260906)
    samples = int(seconds * rate)
    time = np.arange(samples) / rate

    freqs = np.fft.rfftfreq(samples, d=1.0 / rate)
    tilt = np.ones_like(freqs)
    audible = freqs >= 100.0
    tilt[audible] = (freqs[audible] / 100.0) ** -0.75
    signal = np.fft.irfft(np.fft.rfft(generator.standard_normal(samples)) * tilt, n=samples)

    signal *= 0.5 + 0.5 * np.sin(2.0 * np.pi * 3.1 * time)
    signal[: int(0.4 * rate)] = 0.0
    signal[-int(0.4 * rate) :] = 0.0

    return (0.7 * signal / np.abs(signal).max()).astype(np.float32)


def lowpass_by_resample(mono: np.ndarray, rate: int, target: int) -> np.ndarray:
    """Band-limit to `target / 2` with a cutoff that is known rather than asserted."""
    return scipy.signal.resample_poly(scipy.signal.resample_poly(mono, target, rate), rate, target)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--audio", type=pathlib.Path, help="also measure a real clip")
    args = parser.parse_args()

    reference = speech_like()
    full = _bandwidth_hz(reference, RATE)
    print(f"{'condition':38} {'expected':>12} {'measured':>12}   result")
    print(f"{'unfiltered synthetic speech':38} {'~24 kHz':>12} {full / 1000:>10.1f} kHz   -")

    failures = 0
    for target in (8000, 16000, 32000):
        expected = target / 2
        measured = _bandwidth_hz(lowpass_by_resample(reference, RATE, target), RATE)
        ok = measured is not None and abs(measured - expected) <= expected * TOLERANCE
        failures += not ok
        print(
            f"{f'resampled through {target // 1000} kHz':38} "
            f"{expected / 1000:>10.1f} kHz {measured / 1000:>10.1f} kHz   "
            f"{'ok' if ok else 'FAIL'}"
        )

    # A rolloff would rank this signal near its low formants; the point of the detector is that it
    # does not. Guard the property, not just the numbers.
    if full is None or full < 20000.0:
        print(f"FAIL: full-band signal measured {full}, expected near Nyquist (24 kHz)")
        failures += 1

    if args.audio:
        import soundfile as sf

        data, rate = sf.read(str(args.audio), dtype="float32", always_2d=True)
        mono = data.mean(axis=1)
        print(f"\n{args.audio.name}: {_bandwidth_hz(mono, rate) / 1000:.1f} kHz at {rate} Hz")

    print("\n" + ("calibration holds" if not failures else f"{failures} check(s) FAILED"))
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
