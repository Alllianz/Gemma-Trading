import time
import json
from openai import OpenAI

# Conectamos con tu LM Studio
client = OpenAI(base_url="http://localhost:1234/v1", api_key="lm-studio")

# --- SIMULACIÓN DE DATOS (Para jugar "For Fun") ---
# En el futuro, aquí cargas un CSV real de Binance
velas_simuladas = [
    {"open": 64000, "high": 64500, "low": 63900, "close": 64200, "volume": 120},
    {"open": 64200, "high": 64800, "low": 64100, "close": 64700, "volume": 150},
    {"open": 64700, "high": 65200, "low": 64500, "close": 65100, "volume": 180},
    {"open": 65100, "high": 65150, "low": 64300, "close": 64400, "volume": 210},
    {"open": 64400, "high": 64600, "low": 63800, "close": 63900, "volume": 140},
]

saldo_vst = 100000.0  # Tu saldo inicial simulado
posicion_btc = 0.0

system_prompt = """
Eres un agente de trading para BTCUSDT. Tu objetivo es ganar VST.
Analiza las velas históricas que te dará el usuario y decide la siguiente acción.
Debes responder ESTRICTAMENTE en formato JSON con esta estructura:
{
  "analisis": "Tu razonamiento corto",
  "accion": "COMPRAR", "VENDER" o "MANTENER"
}
"""

print("🚀 Iniciando bucle de goteo de velas para Gemma...")

# El bucle de goteo: Le soltamos las velas una a una
for paso, vela in enumerate(velas_simuladas):
    print(f"\n--- [Vela {paso + 1}] Precio Actual: {vela['close']} USDT ---")
    
    prompt_usuario = f"""
    Saldo VST disponible: {saldo_vst}
    Posición actual en BTC: {posicion_btc}
    Última vela recibida: {json.dumps(vela)}
    ¿Qué hacemos? Responde solo en JSON.
    """
    
    try:
        # Llamada a Gemma en tu LM Studio
        response = client.chat.completions.create(
            model="gemma-4-26b-it", # Asegúrate de que este nombre coincida en LM Studio o déjalo vacío si LM Studio autodetecta
            messages=[
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": prompt_usuario}
            ],
            temperature=0.2 # Temperatura baja para que sea más racional
        )
        
        # Procesar la respuesta de Gemma
        respuesta_texto = response.choices[0].message.content
        data = json.loads(respuesta_texto)
        
        print(f"🤖 Gemma dice: {data['analisis']}")
        print(f"📈 Acción elegida: {data['accion']}")
        
        # Simulación básica de la orden
        precio = vela['close']
        if data['accion'] == "COMPRAR" and saldo_vst > 0:
            posicion_btc = saldo_vst / precio
            saldo_vst = 0
            print(f"🛒 COMPRA EJECUTADA: Ahora tienes {posicion_btc} BTC")
        elif data['accion'] == "VENDER" and posicion_btc > 0:
            saldo_vst = posicion_btc * precio
            posicion_btc = 0
            print(f"💰 VENTA EJECUTADA: Tu saldo actual es {saldo_vst} VST")
        else:
            print("⏳ Manteniendo posición...")
            
    except Exception as e:
        print(f"❌ Error en este turno (asegúrate de que LM Studio tenga el servidor encendido): {e}")
        
    time.sleep(3) # Espera 3 segundos antes de "soltarle" la siguiente vela